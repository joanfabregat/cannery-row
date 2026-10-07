# Workflow-spec recovery fixture

`seed.sql` uses fixed serialization identities and clocks in an isolated migrated database. Each observation rolls back independently. The missing approved-revision scenario uses the approved recovery-fixture policy and leaves production constraints intact.
