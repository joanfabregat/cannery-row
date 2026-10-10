-- SPDX-License-Identifier: AGPL-3.0-only
-- Automatic decisions. A science revision's `decide` section says who decides
-- a written-up hypothesis: a researcher (the default), or a decider step that
-- the runner's decide kind runs under a registered `decider` service account.
-- With a decider, the hypothesis's decision case opens as before and a decide
-- job is queued beside it; the decider completes the job with the decision
-- document, recorded on the case with the decider as its actor. A decider
-- promotes only on a `pass` verdict, as a researcher does.

ALTER TABLE jobs DISABLE TRIGGER jobs_frozen;

-- Service accounts.
ALTER TABLE service_accounts DROP CONSTRAINT service_accounts_kind_check;
ALTER TABLE service_accounts ADD CONSTRAINT service_accounts_kind_check
    CHECK (kind IN ('agent', 'experimenter', 'verifier', 'decider'));

-- Decisions: recorded by a researcher, or by a decider service account on a
-- decision case.
ALTER TABLE decisions DISABLE TRIGGER decisions_immutable;
ALTER TABLE decisions ALTER COLUMN actor_user_id DROP NOT NULL;
ALTER TABLE decisions ADD COLUMN actor_service_id uuid REFERENCES service_accounts (id);
-- The revision of the decider step that recorded an automatic decision.
ALTER TABLE decisions ADD COLUMN decider_revision text;
ALTER TABLE decisions ADD CONSTRAINT decisions_actor_check CHECK (
    num_nonnulls(actor_user_id, actor_service_id) = 1
    AND (actor_service_id IS NULL) = (decider_revision IS NULL)
    AND (actor_service_id IS NULL OR (front_matter IS NOT NULL
                                      AND action IN ('promote', 'reject', 'inconclusive',
                                                     'failed'))));
ALTER TABLE decisions ENABLE TRIGGER decisions_immutable;

CREATE OR REPLACE FUNCTION search_index_decision(d decisions) RETURNS void LANGUAGE sql AS $$
    SELECT search_put(c.project_id, 'decision_reason', d.id, NULL, c.hypothesis_id,
                      c.attempt_id, d.actor_user_id, d.actor_service_id, d.action, d.reason,
                      d.decided_at)
    FROM review_cases c WHERE c.id = d.review_case_id AND c.hypothesis_id IS NOT NULL
$$;

-- Decide jobs: one at a time per hypothesis, on its last attempt, performed
-- by the runner under the registered decider; completed with the decision
-- it recorded.
ALTER TABLE jobs ADD COLUMN decision_id uuid REFERENCES decisions (id);
DO $$
DECLARE
    found text;
BEGIN
    SELECT conname INTO STRICT found FROM pg_constraint
    WHERE conrelid = 'jobs'::regclass AND contype = 'c'
      AND pg_get_constraintdef(oid) LIKE '%(evidence_id IS NOT NULL)%';
    EXECUTE format('ALTER TABLE jobs DROP CONSTRAINT %I', found);
END;
$$;
ALTER TABLE jobs ADD CONSTRAINT jobs_output_check CHECK (
    (state = 'completed') = (num_nonnulls(evidence_id, decision_id) = 1)
    AND (decision_id IS NULL OR phase = 'decide')
    AND (evidence_id IS NULL OR phase <> 'decide'));
ALTER TABLE jobs DROP CONSTRAINT jobs_phase_check;
ALTER TABLE jobs ADD CONSTRAINT jobs_phase_check
    CHECK (phase IN ('verify', 'document', 'decide'));
ALTER TABLE jobs DROP CONSTRAINT jobs_performer_check;
ALTER TABLE jobs ADD CONSTRAINT jobs_performer_check CHECK (
    performer IN ('runner', 'agent') AND (performer = 'runner') = (verifier_id IS NOT NULL)
    AND (phase <> 'document' OR performer = 'agent')
    AND (phase <> 'decide' OR performer = 'runner'));

ALTER TABLE jobs ENABLE TRIGGER jobs_frozen;
