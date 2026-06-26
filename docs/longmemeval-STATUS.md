# LongMemEval-S — RESUME HERE (state of play)

> The single **"where are we / what next"** page. Read this first when resuming.
> For the *why* + detailed tiered plan see [`longmemeval-roadmap.md`](longmemeval-roadmap.md); for the research narrative see [`longmemeval-study.md`](longmemeval-study.md).
> **Last updated: 2026-06-20.**

## TL;DR
Durable score is **72.5%** (120-Q, Gemini 2.5 Flash judge). **Retrieval is solved; the bottleneck is answer synthesis.** The latest real win is the **Tier-3 bi-temporal knowledge graph** (knowledge-update 60→80%). A follow-up counting attempt was **inert**, and a re-eval reading 78.3% was **noise**. **Next:** pin the number with a 3× averaged run, then build an **entity-node graph** (fixes counting + unlocks multi-hop) toward the ~90% band.

---

## 1. THIS IS WHERE WE STOPPED  (current state)

- **Score:** **72.5%** (87/120) — durable. A 78.3% follow-up run exists but is noise (see §2).
- **Git:** local `main` = `ef63267` (round notes), **1 commit ahead of `origin/main`** (`bbbf5c3`). **Not pushed.** Author identity is `Akpughe <davidakpughe2@gmail.com>`, no co-authors on any local commit.
- **Committed but inert (gated, no-op):** the **date-windowed counter** — `count_event_instances_tagged` (`src/engine/mod.rs`) + `parse_window` and the `is_count` restructure (`examples/longmemeval.rs`). It **fires 0/120** (the graph has no countable event nodes); kept to be **redone on entity nodes**, not reverted.
- **Data — remote Qdrant (`QDRANT_URL` = `*.nuton.app`, NOT local docker):** collections `ultramem_lme120_chunks` (77.5k), `ultramem_lme120_facts` (63.6k), `ultramem_lme120_graph` (33.7k edges). Per-question namespace tag = `lme_<question_id>`.
- **Result files (gitignored, in `eval/`):** `lme120_results.json` = 63.3% baseline · `lme120b` = 66.7% (prompt pass) · **`lme120g` = 72.5% (graph — current best/durable)** · `lme120h` = 78.3% (noise run, windowed-count was inert).
- **Open housekeeping:** the one remaining item is `80fbf7e` — *already public* with the **old email + a Claude trailer**; scrubbing it needs a force-push.

## 2. THIS IS WHAT WE ACHIEVED

**Trajectory (trustworthy 120-Q, 20/category):** 63.3% baseline → 66.7% (type-aware answer prompts) → **72.5% (Tier-3 graph)**.

| Category | 72.5% (current) |
|---|---|
| single-session-user | 90% |
| single-session-assistant | 70% |
| single-session-preference | 45% |
| knowledge-update | **80%** |
| temporal-reasoning | 85% |
| multi-session | 65% |

**The wins that stuck (reproducible mechanisms, not noise):**
- **Retrieval solved** — 97.5% of gold sessions retrieved (round-level chunking + fact-augmented keys + query decomposition).
- **Single-session recall solved** (user/assistant recall was the early hole).
- **Event-time facts** (`[on YYYY-MM-DD]`) eliminated wrong-date temporal errors.
- **Tier-3 bi-temporal graph deterministically fixed knowledge-update 60→80%** — the headline durable win (the "use the latest value" cases prompting could not crack: 5K personal best, yoga/therapist/cocktail-class frequency).

**Hard-won lessons (the things we must not forget):**
- **Architecture > model** (Gemini ≈ Groq; a frontier model moved the needle ~0).
- **More context hurts** (lost-in-the-middle: full-sessions-everywhere regressed 63→47%).
- **Answer-model noise is ±5 on volatile categories** — preference swung 45→70% with *no* code change. A single 120-Q run can't measure a few-point delta or catch a small regression.
- **Preference is a judge/subjectivity ceiling** — ~half its "failures" are defensible answers; don't hard-engineer it.
- **Counting distinct events needs entity *nodes*** — the entity-*attribute* graph can't count (weddings scatter across `wedding_venue`/`_month`/`_role`).

## 3. THIS IS WHAT WE'RE TRYING TO GET TO  (goal)

- **The leaders' band — ~88–92%.** The proven route is a **temporal knowledge graph** (Zep/Graphiti = **90.2%** on LongMemEval).
- **Caveat on "near-perfect":** judge artifacts mean our *effective* score is already a few points above the printed 72.5%; some categories (preference) have a subjective ceiling below 100%.

## 4. THESE ARE THE THINGS WE'LL DO TO GET THERE  (next steps, in order)

1. **Pin the number — run the eval 3× and average.** Controls the ±5 noise so we can actually measure changes and detect regressions. *Prerequisite for everything below.* (eval-only, cheap; command in §How-to.)
2. **Entity-node knowledge graph** — promote events/people/places to *nodes* (each wedding = one node). This **fixes multi-session counting** AND **unlocks multi-hop relationship traversal** — the Zep recipe and the real lever toward ~90%. Needs a graph re-ingest/backfill (the inert counter code folds into this).
3. **Fold the (committed, gated) counter code into the entity-node work (#2)** — `parse_window` + `count_event_instances_tagged` are the seed; they need countable event nodes to fire.
4. **(Optional) Leaderboard-faithful judge** — add a GPT-4o judge path for official-comparable numbers; stop tuning preference.
5. **Housekeeping** — push `ef63267`; fix `80fbf7e` (force-push) if you want public history fully clean.

---

## How to verify state / resume in 2 minutes

```bash
# 1. Where the code is
git log --oneline -5 && git status -s        # newest docs/report/charts on top; tree clean

# 2. The durable result (72.5%) — per-category
python3 -c "import json,collections as c; d=json.load(open('eval/lme120g_results.json')); t=c.Counter(r['type'] for r in d if r['correct']); n=c.Counter(r['type'] for r in d); [print(k, t[k],'/',n[k]) for k in n]"

# 3. The graph data is intact (reuse for re-eval; NO re-ingest needed)
#    (creds in .env: QDRANT_URL is the remote *.nuton.app, value is quoted)

# 4. Re-run the eval (eval-only, ~20-40 min) — this is the Tier-3 config
ULTRAMEM_LME_MODE=eval \
  ULTRAMEM_CHUNKS_COLLECTION=ultramem_lme120_chunks \
  ULTRAMEM_FACTS_COLLECTION=ultramem_lme120_facts \
  ULTRAMEM_GRAPH_COLLECTION=ultramem_lme120_graph \
  ULTRAMEM_LME_OUT=eval/lme120_run.json \
  cargo run --release -p ultramem-core --example longmemeval -- 20 eval/longmemeval_s.json
```

**One-line status:** *Solved retrieval; graph cracked knowledge-update (72.5%); now noise-limited — pin the number, then go entity-node graph for counting + multi-hop toward ~90%.*
