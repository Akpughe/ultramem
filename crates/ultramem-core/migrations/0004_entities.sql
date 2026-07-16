-- 9/10 entity resolution (slice 9b): per-namespace canonical-entity aliases.
--
-- Within a container_tag, a normalized surface form (`alias`) maps to a canonical
-- entity name. Resolution is EXPLICIT — only registered aliases unify; an unknown
-- name resolves to itself. Additive: nothing reads this yet at the hot path; it
-- backs the /v1/entities admin + resolve endpoints.
create table if not exists entity_aliases (
    id            bigserial primary key,
    container_tag text   not null,
    alias         text   not null,   -- normalized surface form (see entity::normalize)
    canonical     text   not null,   -- canonical entity name
    created_at    bigint not null,
    unique (container_tag, alias)
);
create index if not exists entity_aliases_tag on entity_aliases (container_tag);
