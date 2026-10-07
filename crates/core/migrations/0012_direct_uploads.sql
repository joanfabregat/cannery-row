-- Direct uploads to an S3-compatible store. A grant's transfer says how its
-- bytes arrive: streamed through the API (the only way with the local
-- store), in one presigned PUT, or in presigned parts of a multipart upload
-- the API created (multipart_upload_id) with a fixed part size. A direct
-- upload ends with a finish request, which verifies the stored object.
ALTER TABLE uploads ADD COLUMN transfer text NOT NULL DEFAULT 'stream'
    CHECK (transfer IN ('stream', 'single', 'multipart'));
ALTER TABLE uploads ADD COLUMN multipart_upload_id text;
ALTER TABLE uploads ADD COLUMN part_size bigint CHECK (part_size > 0);
ALTER TABLE uploads ADD CONSTRAINT uploads_multipart_check CHECK (
    (transfer = 'multipart') = (multipart_upload_id IS NOT NULL)
    AND (transfer = 'multipart') = (part_size IS NOT NULL)
);

-- When the last presigned URL issued for the grant expires. Until then a
-- client may still write the key, so a failed grant's bytes stay pending
-- delete (object_pending_delete) until the sweep deletes them after it.
ALTER TABLE uploads ADD COLUMN urls_expire_at timestamptz;

-- The logical key a grant reserves (its role and name). A streamed upload
-- writes its slot; a direct upload writes a key unique to the grant, so a
-- presigned PUT still in flight from an expired grant can never overwrite
-- what a replacement grant verified. An expired direct grant is retired
-- (slot cleared) rather than replaced in place, so the sweep still finds
-- its own key and multipart upload.
ALTER TABLE uploads ADD COLUMN slot text;
UPDATE uploads SET slot = key;
CREATE UNIQUE INDEX uploads_slot_key ON uploads (backend, bucket, slot);
ALTER TABLE uploads ADD CONSTRAINT uploads_slot_check CHECK (slot IS NOT NULL OR transfer <> 'stream');
