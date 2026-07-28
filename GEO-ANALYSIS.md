# Ephemeral Sandbox GEO and SEO analysis

**Analysis date:** 2026-07-28
**Public surface reviewed:** `https://github.com/Ephemeral-AI-Lab/ephemeral-sandbox`
**Method:** static analysis of the public repository and its README, plus GitHub
repository metadata. Scores are directional heuristics, not Search Console or
AI-platform telemetry.

## Readiness: 58/100

| Platform | Score | Evidence and limitation |
|---|---:|---|
| Google Search and AI Overviews | 62 | The README is rendered server-side by GitHub and has clear product headings, but the repository has no description, homepage, topics, or standalone indexable site. |
| ChatGPT web search | 56 | The public GitHub source and documentation are discoverable, but the project lacks a root-level `llms.txt` endpoint on a domain it controls and has limited external entity signals. |
| Perplexity | 54 | The source, documentation, and Discord link provide useful primary material; no public product site, original research, or community evidence is available from this audit. |

## Findings addressed in this change

1. The README now begins with a self-contained definition of Ephemeral Sandbox.
   It directly answers a likely product query and describes the product, users,
   isolation model, interfaces, and supported host platforms in one extractable
   passage.
2. Question-led section headings and a focused use-case section make the README
   easier to retrieve for agent-sandbox and parallel-agent queries.
3. `llms.txt` supplies an authoritative map of the product, documentation,
   architecture, console, terminology, and key facts for a future domain or
   documentation deployment.
4. `.seo-cache/` is ignored so local analysis data remains uncommitted.

## Technical accessibility

- GitHub renders the README server-side, so the core product description is
  available without a client-side JavaScript dependency.
- The browser console is a local operator UI, not a public product website; it
  is therefore not an indexable acquisition page.
- GitHub owns the active `robots.txt`; this repository cannot independently
  permit or deny AI crawlers on `github.com`.
- The repository now contains `llms.txt`, but GitHub does not expose it at
  `https://github.com/llms.txt`. It becomes directly useful once deployed at
  `https://<product-domain>/llms.txt`.

## Highest-impact follow-up actions

1. Set the GitHub repository description to: **Open-source local workspace
   isolation for parallel coding agents, with Docker, CLI, MCP, and
   observability.**
2. Add relevant GitHub topics: `ai-agents`, `coding-agents`, `agent-sandbox`,
   `mcp`, `model-context-protocol`, `developer-tools`, `rust`, and `docker`.
3. Launch a canonical public documentation or product domain and serve the
   versioned `llms.txt` at its root. Set the repository homepage to that URL.
4. Publish 2–3 source-backed technical guides that answer high-intent queries:
   *how to isolate parallel coding agents*, *MCP sandbox for coding agents*,
   and *reviewing agent changes before merge*.
5. Add Organization and SoftwareApplication JSON-LD, a canonical URL,
   `robots.txt`, sitemap, author/reviewer dates, and `sameAs` links only on the
   future product domain—not in this GitHub repository.

## Citation and authority plan

Use the architecture documentation, CLI/MCP references, reproducible benchmarks,
and release notes as primary sources. Prefer concrete claims about the isolation
model, supported interfaces, and operating systems; avoid unverified claims about
security guarantees, performance, or compatibility. Build third-party mentions
through release announcements, technical demonstrations, and user discussions
rather than synthetic backlinks.
