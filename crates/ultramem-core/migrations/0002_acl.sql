-- 8/10 scopes/ACLs (slice 8a): explicit access grants.
--
-- A "scope" is a container_tag (an individual, team, project, account, or company
-- namespace). An acl_entry grants a principal (a credential/tenant) a capability
-- on a scope. Visibility is FAIL-CLOSED: a principal sees only its own scope plus
-- scopes it has been explicitly granted read (or higher). No implicit inheritance.
--
-- Scaffold only — nothing enforces these yet; the retrieval wiring is a later,
-- separately-reviewed slice.

create table if not exists acl_entries (
    id         bigserial primary key,
    principal  text   not null,   -- credential/tenant id (the acting party)
    scope      text   not null,   -- container_tag the grant applies to
    capability text   not null,   -- read | write | promote | admin
    created_at bigint not null,
    unique (principal, scope, capability)
);
create index if not exists acl_entries_principal on acl_entries (principal);
