-- Job outputs checked against their interface. A job upload may name the
-- interface of the step output it carries; the API then checks the bytes
-- against it while they stream in and records whether the content itself
-- (JSON or JSON Lines, under storage.validate_json_max_bytes) was validated.
-- Both stay NULL for an upload without an interface.
ALTER TABLE uploads ADD COLUMN interface text;
ALTER TABLE artifacts ADD COLUMN interface text;
ALTER TABLE artifacts ADD COLUMN content_validated boolean;
ALTER TABLE artifacts ADD CONSTRAINT artifacts_content_validated_needs_interface
    CHECK (content_validated IS NULL OR interface IS NOT NULL);

-- The problems of an upload the API itself refused against its interface:
-- the server-held evidence that blames a producer's invalid output on the
-- agent. NULL unless the upload was refused.
ALTER TABLE uploads ADD COLUMN refusal jsonb;
ALTER TABLE uploads ADD CONSTRAINT uploads_refusal_needs_failure
    CHECK (refusal IS NULL OR (state = 'failed' AND interface IS NOT NULL));

-- A failed upload whose bytes may still sit under its key: the request that
-- failed it deletes them and clears the flag; if that delete fails, the
-- sweep deletes them (unless the key holds a verified artifact).
ALTER TABLE uploads ADD COLUMN object_pending_delete boolean NOT NULL DEFAULT false;
CREATE INDEX uploads_object_pending_delete ON uploads (id) WHERE object_pending_delete;
