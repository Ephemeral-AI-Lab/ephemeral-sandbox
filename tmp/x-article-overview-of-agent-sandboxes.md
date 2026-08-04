# Overview of Agent Sandboxes in Practice

*Adapted from [Part 0 of the Agent Infrastructure Book](https://github.com/agent-infra-foundation/agent-infra-book/blob/main/sandbox/ephemeral-sandbox/volume-1/chapters/PART-0.md)*

---

“Agent sandbox” sounds like one product category. In practice it’s a whole toolbox: permission rules, private workspaces, containers, microVMs, browser sessions, disposable cloud machines, checkpoints, and fleet control.

That’s why people talk past each other. One team means “don’t let the model trash my laptop.” Another means “give me a clean Linux box for twenty minutes.” A third means “let ten coding agents try patches without stomping on each other.” Same word. Different jobs.

Follow one simple story — a few coding agents updating a dependency — and you can see what each layer protects, and what it quietly does *not* promise.

---

## Why personal computers fall apart at agent scale

Three agents edit the same checkout. They overwrite each other’s lockfiles. They fight over the same port for test servers. Each reports “progress.” Nobody can explain the final mess.

The OS did nothing wrong. One user *can* start processes and edit files. What broke is human coordination — the informal “I’ll take that file” that works when one person is in charge.

At agent scale, each attempt needs private execution, a clear identity and starting point, history of what happened, and a controlled way to return results into the shared project.

Two pieces often get blurred:

- A **sandbox environment** holds tools, processes, browser state, and workspace.
- An **agent runtime** is the brain: call the model, pick the next action, take in the result, repeat.

They cooperate. They are not the same thing.

**Core idea:** the sandbox holds *execution state*. The agent runtime owns the *decision loop*.

---

## Only two places the “brain” can live

Ask one question: where does the model → tool → observation → next model call loop run?

**There are only two answers.**

**1. In-sandbox agent** — The runtime lives *inside* the environment with its tools and workspace. Calling a remote model doesn’t change that. If the thing choosing tools sits inside, it’s in-sandbox.

**2. External agent with sandboxed tools** — The runtime sits *outside* and sends work through a protocol. Something inside runs commands (shell, PTY, MCP surface), but that worker is not another agent — it doesn’t own the decision loop.

Neither always wins. In-sandbox feels natural with local CLIs and files, but lifecycle and credentials can glue to the environment. External keeps history and credentials outside generated code and can swap environments more easily — but the protocol must handle streaming, long processes, disconnects, and safe retries.

“Sandbox-as-a-Service” is a delivery model, not a third placement mode. Anthropic’s Managed Agents picture is external: an outside harness owns decisions; a replaceable worker acts as the hands.

---

## A private folder is not enough

Separate directories fix file collisions. Harder questions remain: Can it read SSH keys? Can two servers bind the same port? Which base revision produced this? Can a failed attempt resume? Who is allowed to publish?

Those surface six pressures:

| Pressure | What it needs |
| --- | --- |
| **Concurrency** | Ownership of files, processes, ports |
| **Auditability** | Trail from identity + base → actions → outputs |
| **Cost** | Fast create / pack / pause / clean up |
| **Reproducibility** | Named templates and bases — not vague “fresh” |
| **Recovery** | Snapshots, forks, or replay |
| **Publication** | A real decision that turns private results into shared truth |

“Looks fine and tests passed” isn’t enough for trust. You want structured causality: which agent, which session, which base, which commands, which exact proposed bytes.

Security cuts across all of this — but “contains hostile code” is too narrow. One system can wall off a guest kernel and still have no useful return path. Another can be excellent for cooperating coding agents without claiming multi-tenant isolation.

Whenever you hear “sandbox,” ask for four things: **unit**, **boundary**, **state model**, and **return path**.

---

## Coding agents: desks, rules, and locked rooms

Three Git worktrees stop agents from overwriting files. They may still share home directory, credentials, kernel, network, and local services.

A worktree is a **private desk**, not a **locked room**.

Coding products stack boundaries people blur:

- **Permission policy** — what needs approval  
- **Private workspace** — separate unfinished edits  
- **Process sandbox** — restrict filesystem / process / network  
- **Container** — isolate resources, usually still share the host kernel  
- **VM / microVM** — add a guest-kernel wall  

These compose. They are not one universal security score.

How real products split:

- **Codex** and **Claude Code** (local) keep the agent runtime outside and sandbox *commands* — external agent with sandboxed tools. Worktrees reduce edit collisions; they are not a tenant wall. Cloud modes often: container → checkout → setup → run → answer + diff.
- **Docker Sandboxes** put the agent *inside* a microVM — in-sandbox. Stronger host boundary. Still not automatically “how do we merge competing patches?”
- **Gemini CLI** can use a container, gVisor, or tool-level sandboxing. The product name doesn’t fix the boundary; the mechanism does.

| Mechanism | Main question |
| --- | --- |
| Worktree / private workspace | Whose files? |
| Command sandbox | Which operations? |
| MicroVM | How exposed is the host? |

None of those alone decides whether the result returns as a commit, a patch, or an artifact. Ten private worktrees can stop file stomping while still fighting over CPU, ports, and API rate limits.

---

## Cloud sandboxes: a computer with a lifecycle API

“Give this agent a clean Linux computer for twenty minutes” sounds simple — until the run times out after tests pass, before the patch is downloaded. Isolated machine. Lost work.

Think of an on-demand cloud sandbox as a **computer with explicit lifecycle**: template → allocate → run → pause/snapshot → destroy → return selected outputs.

**Lifecycle rule:** isolation protects a *running* sandbox; retention and return protect the *work* it produces.

“Fresh” can mean a new process, a clean disk, a cached template, a restored snapshot, or a resumed machine. Those are not the same. Reproducibility means **naming the base**.

Services like E2B, Daytona, and Modal usually sit in external-agent mode: APIs for create / connect / pause / snapshot / fork, returning files, command results, URLs, or snapshots.

**Common non-guarantee:** a task sandbox returns *material*, not *shared project truth*. Stdout can say tests passed while the useful patch is still trapped inside. Restoring files ≠ restoring a running process. The caller still needs capture, comparison, and publication.

---

## Browser sandboxes: more than what’s on screen

Tests pass. An agent logs into staging, changes a preference, downloads a report, hands off for human payment confirmation. Closing the browser doesn’t answer what should persist.

Browser sandboxes carry cookies, storage, downloads, auth, and display state. A disposable session can still attach to a durable profile.

**State rule:** the browser process can be throwaway while login, evidence, and downloads stick around.

Browserbase-style systems often start fresh and optionally keep a Context across sessions. AIO Sandbox-style systems pack browser, shell, files, and tools into one container — convenient, but one container ≠ strong multi-tenant isolation or a clean code-publication flow.

Trap: **a screenshot is an observation, not the state.** Two pages can look identical while cookies and permissions differ. Never treat “I saw it on screen” as “we approved this project change.”

---

## RL, checkpoints, and control planes (the short version)

**RL / evaluation** wants many private attempts from one initialized root — bootstrap once, fork branches, score leaves, record the exact checkpoint behind every reward. Fast checkpoint systems (CubeSandbox, research like DeltaBox) speed reuse; they are not the search policy, verifier, or trainer. File rollback doesn’t undo a remote DB write. Shared caches between branches corrupt rewards.

**Checkpoints save different ground.** A workspace snapshot keeps files. A process checkpoint may keep memory too. A changeset captures a proposal against a named base. **None undoes an email, payment, or leaked credential.** A checkpoint rewinds only what it captured; publication is a separate decision against shared history that may have moved on.

**Control planes are three jobs, not one:**

1. **Meta-agent** — observe or redirect another agent’s decisions  
2. **Lifecycle control plane** — create, pause, restore, destroy environments  
3. **Scheduler** — place work on machines and capacity  

A daemon that runs a command is still an execution worker, not a placement mode. Warm pools must never keep a previous owner’s private workspace.

---

## Publication: finishing is not landing

Three plausible fixes. All pass private tests. Isolation kept attempts clean. **It did not decide which belongs in the project.**

**Publication rule:** finishing creates a private result; publishing makes an accepted result *shared truth*.

| Return form | Roughly means |
| --- | --- |
| Stdout / value | One operation’s answer |
| File / artifact | Selected bytes |
| Patch | Edits vs an assumed base |
| Branch | Commits + history |
| Changeset | Mutations + explicit base + identity |
| Publication | All-or-reject into shared history |

Classic race: A and B start from the same base; B publishes first; blindly applying A erases B even if A’s private tests passed. A changeset lets you compare, then reject, resolve, or prepare a new proposal without throwing away evidence.

A clean contract separates four events:

1. **Finish** — agent stopped  
2. **Capture** — private mutations become a reviewable proposal  
3. **Resolve** — compare with current shared base and policy  
4. **Publish** — accepted set becomes shared history (or reject and keep it inspectable)

---

## Where Ephemeral Sandbox fits

[Ephemeral Sandbox](https://github.com/Ephemeral-AI-Lab/ephemeral-sandbox) sits at this **return boundary**.

Multiple coding agents work in isolated workspace sessions over one stable project base. Each session has private writable state. The runtime can capture a reviewable changeset, resolve it against shared history, and publish with provenance. CLI and MCP control the runtime; observability records the transition.

- **Unit:** one shared sandbox, private workspace sessions  
- **Placement:** external agent via control surfaces (in-sandbox remains possible)  
- **Isolation:** for *cooperating* coding agents, not hostile multi-tenant walls  
- **Lifecycle:** create from an explicit base → execute → capture → publish or reject → destroy  
- **Return path:** changeset → all-or-reject publication  
- **Non-guarantee:** v1 workspace isolation is **not** a hardened microVM for mutually untrusted tenants  

That limit is the point, not a footnote. Parallel private workspaces and controlled publication are real problems. Hostile multi-tenant code still needs the right container/VM/microVM, credentials, network policy, and fleet layer. Publication **composes** with those systems; it does not replace them.

---

## Don’t inflate one box into “the agent computer”

Adding a microVM does not make publication conflict-aware.  
Adding provenance does not stop kernel exploits.  
Adding process checkpointing does not revoke a leaked token.  
Adding a scheduler does not turn an execution worker into an agent runtime.

**The useful architecture is not one giant box labeled “agent sandbox.”** It’s a set of boundaries you can check independently — then compose.

| Layer | Typical job |
| --- | --- |
| Policy / command sandbox | What may run without approval |
| Workspace isolation | Private unfinished work |
| Container / microVM | Host and tenant exposure |
| Cloud lifecycle | Disposable computers + retention |
| Browser session / profile | UI automation + auth state |
| Checkpoint / branch | Explore and recover |
| Control plane / fleet | Place, replace, warm-pool |
| Changeset + publication | Land accepted work into shared truth |

Next time someone says “we need a sandbox for agents,” ask:

1. **Unit** — what is one sandbox?  
2. **Placement** — where does the decision loop run?  
3. **Boundary** — files, processes, network, kernel, or policy?  
4. **State** — what’s private, what’s the base, what survives pause?  
5. **Return** — stdout, files, patch, branch, or publication?  
6. **Non-guarantee** — what does this explicitly *not* claim?

Answer those six, and “agent sandbox” stops being a buzzword and starts being a design you can actually build — and trust.

---

*Source: Part 0 of the [Agent Infrastructure Book](https://github.com/agent-infra-foundation/agent-infra-book) — isolation, state, rollouts, recovery, and publication across agent runtimes. Full diagrams and product tables live there.*
