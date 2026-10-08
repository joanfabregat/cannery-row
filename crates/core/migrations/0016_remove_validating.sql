-- SPDX-License-Identifier: AGPL-3.0-only
-- `validating` was declared as an attempt state but no transition ever set
-- it: the submission moves an attempt straight to `testing`. It goes.
ALTER TABLE attempts DROP CONSTRAINT attempts_state_check;
ALTER TABLE attempts ADD CONSTRAINT attempts_state_check CHECK (state IN (
    'claimed', 'running', 'submitted', 'testing', 'evaluating',
    'awaiting_human_review', 'promoted', 'rejected', 'inconclusive', 'failed', 'cancelled',
    'unreviewed'));
