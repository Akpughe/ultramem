-- Phase A — Postgres source of truth (Task 1: initial schema).
--
-- Ids are TEXT holding the same UUID strings the engine already mints for Qdrant
-- point ids, so a row and its vector share an id with zero conversion. Times are
-- BIGINT unix-epoch seconds to match the engine's existing `captured_at`.
-- Nothing writes here yet — this is the scaffold; the engine dual-writes in a
-- later slice.

create table if not exists documents (
    id                  text primary key,
    container_tag       text   not null,
    source              text   not null,
    title               text   not null default '',
    reference           text   not null default '',
    canonical_url       text,
    content_hash        text,
    blob_key            text,
    captured_at         bigint not null,
    source_published_at bigint,
    processing_state    text   not null default 'pending',
    error               text,
    created_at          bigint not null
);
create index if not exists documents_tag_time on documents (container_tag, captured_at desc);
create unique index if not exists documents_tag_url on documents (container_tag, canonical_url);
create unique index if not exists documents_tag_hash on documents (container_tag, content_hash);

create table if not exists chunks (
    id          text primary key,
    document_id text not null references documents(id) on delete cascade,
    chunk_index int  not null,
    content     text not null,
    char_start  int,
    char_end    int,
    embed_model text not null,
    dim         int  not null
);
create index if not exists chunks_doc on chunks (document_id);

create table if not exists memories (
    id            text primary key,
    container_tag text    not null,
    kind          text    not null default 'unknown',
    statement     text    not null,
    confidence    real,
    is_latest     boolean not null default true,
    needs_review  boolean not null default false,
    supersedes    text,
    superseded_by text,
    extends       text,
    event_from    bigint,
    event_to      bigint,
    valid_until   bigint,
    learned_at    bigint  not null,
    document_id   text    not null references documents(id) on delete cascade,
    created_at    bigint  not null
);
create index if not exists memories_active on memories (container_tag, is_latest, needs_review);

create table if not exists memory_evidence (
    id          text primary key,
    memory_id   text not null references memories(id) on delete cascade,
    document_id text not null references documents(id) on delete cascade,
    chunk_id    text,
    char_start  int,
    char_end    int,
    quote       text not null,
    extractor   text not null
);
create index if not exists memory_evidence_memory on memory_evidence (memory_id);

create table if not exists jobs (
    id            text primary key,
    container_tag text,
    kind          text   not null,
    state         text   not null default 'queued',
    progress      int    not null default 0,
    total         int,
    error         text,
    created_at    bigint not null,
    updated_at    bigint not null
);

create table if not exists audit_events (
    id            bigserial primary key,
    actor         text   not null,
    container_tag text,
    action        text   not null,
    target_id     text,
    request_id    text,
    ts            bigint not null
);
create index if not exists audit_events_tag_time on audit_events (container_tag, ts desc);
