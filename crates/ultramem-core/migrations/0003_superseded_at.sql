-- 9/10 temporal (slice 9a): transaction-time bitemporal `as_of`.
--
-- Record WHEN a memory stopped being current (was superseded), so a point-in-time
-- query can reconstruct "what we knew as of time T" — not just the present. NULL
-- means never superseded (still the latest we know). Pre-existing rows are NULL,
-- i.e. treated as current, which is correct for data captured before this column.
alter table memories add column if not exists superseded_at bigint;
create index if not exists memories_superseded_at on memories (container_tag, superseded_at);
