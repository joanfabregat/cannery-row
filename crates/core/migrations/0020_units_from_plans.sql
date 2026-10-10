-- SPDX-License-Identifier: AGPL-3.0-only
-- Hypotheses come only from approved plans: none is drafted, revised before
-- approval or declined on its own any more. Earlier drafts are cancelled,
-- and declined hypotheses are kept as cancelled with the reason they were
-- declined; the audit log records each change. Draft review cases and their
-- decisions are removed: the audit log keeps what they said.

INSERT INTO audit_events (project_id, actor_kind, via_channel, action, subject_type, subject_id,
                          prior_state, new_state, reason)
SELECT h.project_id, 'system', 'system', 'hypothesis.cancelled', 'hypothesis', h.id::text,
       jsonb_build_object('state', h.state), jsonb_build_object('state', 'cancelled'),
       CASE h.state
           WHEN 'draft' THEN 'Hypotheses now come only from approved plans; this draft was never approved.'
           ELSE 'Declined: ' || coalesce((
               SELECT d.reason FROM decisions d JOIN review_cases c ON c.id = d.review_case_id
               WHERE c.hypothesis_id = h.id AND c.kind = 'draft' AND d.action = 'decline'
               ORDER BY d.decided_at DESC LIMIT 1), 'no reason was recorded.')
       END
FROM hypotheses h
WHERE h.state IN ('draft', 'declined')
ORDER BY h.project_id, h.number;

UPDATE hypotheses SET state = 'cancelled', updated_at = now()
WHERE state IN ('draft', 'declined');

DELETE FROM search_documents s USING decisions d, review_cases c
WHERE s.kind = 'decision_reason' AND s.source_id = d.id
  AND d.review_case_id = c.id AND c.kind = 'draft';
ALTER TABLE decisions DISABLE TRIGGER decisions_immutable;
DELETE FROM decisions d USING review_cases c
WHERE d.review_case_id = c.id AND c.kind = 'draft';
ALTER TABLE decisions ENABLE TRIGGER decisions_immutable;
DELETE FROM review_cases WHERE kind = 'draft';

ALTER TABLE hypotheses ALTER COLUMN state DROP DEFAULT;
DO $$
DECLARE
    name text;
BEGIN
    FOR name IN
        SELECT conname FROM pg_constraint
        WHERE conrelid = 'hypotheses'::regclass AND contype = 'c'
          AND pg_get_constraintdef(oid) LIKE '%draft%'
    LOOP
        EXECUTE format('ALTER TABLE hypotheses DROP CONSTRAINT %I', name);
    END LOOP;
END;
$$;
ALTER TABLE hypotheses ADD CONSTRAINT hypotheses_state_check CHECK (state IN (
    'queued', 'active', 'awaiting_human_review', 'promoted', 'rejected', 'inconclusive',
    'failed', 'cancelled'));
-- A hypothesis is approved when it is created, except one cancelled before
-- plans wrote every hypothesis.
ALTER TABLE hypotheses ADD CONSTRAINT hypotheses_approved_check
    CHECK (state = 'cancelled' OR (approved_revision IS NOT NULL AND approved_at IS NOT NULL));

DROP INDEX review_cases_one_pending_draft_idx;
ALTER TABLE review_cases DROP CONSTRAINT review_cases_kind_check;
ALTER TABLE review_cases ADD CONSTRAINT review_cases_kind_check
    CHECK (kind IN ('plan', 'result', 'failure'));
ALTER TABLE review_cases DROP CONSTRAINT review_cases_attempt_check;
ALTER TABLE review_cases ADD CONSTRAINT review_cases_attempt_check
    CHECK ((kind = 'plan') = (attempt_id IS NULL));
ALTER TABLE decisions DROP CONSTRAINT decisions_action_check;
ALTER TABLE decisions ADD CONSTRAINT decisions_action_check CHECK (action IN (
    'approve', 'send_back', 'decline', 'promote', 'reject', 'inconclusive', 'retry',
    'close_failed'));
