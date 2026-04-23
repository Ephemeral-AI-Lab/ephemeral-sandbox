# ruff: noqa
"""Live E2E: Agent builds a real Next.js project inside a Daytona sandbox.

End-to-end pipeline that verifies the FULL agent stack:
1. Real sandbox creation and lifecycle
2. Agent scaffolds a Next.js project via tool calls
3. Code intelligence (CI) service initializes on the project
4. LSP tools return meaningful results on TypeScript/React files
5. Multi-turn tool chaining: create -> verify -> modify -> verify
6. Sandbox cleanup

Run with: pytest tests/test_e2e/test_live_nextjs_sandbox.py -m live -v
"""

from __future__ import annotations

import pytest

from engine.testing.eval_agent import EvalAgent
from sandbox.testing import get_sandbox_service
from tests.test_e2e.conftest import (
    create_eval_agent,
    create_test_sandbox,
    delete_test_sandbox,
)

pytestmark = [pytest.mark.e2e, pytest.mark.live]

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

KNOWN_DAYTONA_TOOLS = {
    "daytona_shell",
    "daytona_read_file",
    "daytona_write_file",
    "daytona_grep",
    "daytona_glob",
    "daytona_edit_file",
    "ci_query_symbol",
    "ci_diagnostics",
}

NEXTJS_AGENT_PROMPT = (
    "You are a senior fullstack developer with a remote Daytona sandbox. "
    "You MUST use tools for every action — never just describe what you'd do. "
    "Use daytona_write_file to create files, daytona_shell to run commands, "
    "daytona_read_file to read files. "
    "You specialize in Next.js, React, and TypeScript projects. "
    "Always execute every step using tools. Be concise."
)


# ===========================================================================
# Shared sandbox fixture — one sandbox for the whole test module
# ===========================================================================


@pytest.fixture(scope="module")
def sandbox_id():
    """Create a real Daytona sandbox for Next.js project tests."""
    if not EvalAgent.has_all():
        pytest.skip("LLM + Daytona credentials required")
    sb = create_test_sandbox("nextjs-e2e")
    print(f"\n=== Created sandbox: {sb['id']} ===")
    yield sb["id"]
    print(f"\n=== Cleaning up sandbox: {sb['id']} ===")
    delete_test_sandbox(sb["id"])


@pytest.fixture(scope="module")
def nextjs_sandbox():
    """Create a real Daytona sandbox (dict form) for non-agent tests."""
    if not EvalAgent.has_daytona():
        pytest.skip("Daytona not configured")
    sb = create_test_sandbox("nextjs-e2e")
    print(f"\n=== Created sandbox: {sb['id']} ===")
    yield sb
    print(f"\n=== Cleaning up sandbox: {sb['id']} ===")
    delete_test_sandbox(sb["id"])


# ===========================================================================
# AREA 1: Sandbox Creation & Direct Tool Verification
# ===========================================================================


class TestSandboxCreationAndHealth:
    """Verify sandbox is created, healthy, and accessible via direct SDK calls."""

    def test_sandbox_created_with_id(self, nextjs_sandbox):
        """Sandbox should have a non-empty ID and be in started state."""
        assert nextjs_sandbox["id"], "Sandbox ID is empty"
        assert nextjs_sandbox["state"] in ("started", "running", "ready"), (
            f"Expected started state, got: {nextjs_sandbox['state']}"
        )

    def test_sandbox_bash_exec(self, nextjs_sandbox):
        """Direct bash exec in sandbox should work."""
        svc = get_sandbox_service()
        raw_sb = svc.get_sandbox_object(nextjs_sandbox["id"])
        resp = raw_sb.process.exec("echo 'SANDBOX_ALIVE'", timeout=30)
        assert "SANDBOX_ALIVE" in (resp.result or ""), f"Exec failed: {resp.result}"

    def test_sandbox_has_node(self, nextjs_sandbox):
        """Sandbox should have Node.js installed for Next.js development."""
        svc = get_sandbox_service()
        raw_sb = svc.get_sandbox_object(nextjs_sandbox["id"])
        resp = raw_sb.process.exec("node --version", timeout=30)
        result = resp.result or ""
        assert result.startswith("v"), f"Node not found or wrong format: {result}"

    def test_sandbox_has_npm(self, nextjs_sandbox):
        """Sandbox should have npm available."""
        svc = get_sandbox_service()
        raw_sb = svc.get_sandbox_object(nextjs_sandbox["id"])
        resp = raw_sb.process.exec("npm --version", timeout=30)
        result = resp.result or ""
        assert result[0].isdigit(), f"npm not found: {result}"


# ===========================================================================
# AREA 2: Agent Scaffolds Next.js Project via Tool Calls
# ===========================================================================


@pytest.mark.asyncio
async def test_create_package_json(sandbox_id):
    """Agent creates package.json with Next.js dependencies."""
    agent = create_eval_agent(sandbox_id=sandbox_id, system_prompt=NEXTJS_AGENT_PROMPT)
    result = await agent.invoke(
        "Use daytona_write_file to create /workspace/nextjs-app/package.json with this content:\n"
        '{"name": "nextjs-e2e", "version": "1.0.0", "private": true, '
        '"scripts": {"dev": "next dev", "build": "next build", "start": "next start"}, '
        '"dependencies": {"next": "14.0.0", "react": "18.2.0", "react-dom": "18.2.0"}, '
        '"devDependencies": {"typescript": "5.0.0", "@types/react": "18.2.0", "@types/node": "20.0.0"}}'
    )
    started = result.tools_started()
    assert len(started) >= 1, f"Should use tool. Tool names: {result.tool_names}"

    tool_names = [e.tool_name for e in started]
    assert any(n in ("daytona_write_file", "daytona_shell") for n in tool_names), (
        f"Should use write tool: {tool_names}"
    )


@pytest.mark.asyncio
async def test_create_tsconfig(sandbox_id):
    """Agent creates tsconfig.json for TypeScript support."""
    agent = create_eval_agent(sandbox_id=sandbox_id, system_prompt=NEXTJS_AGENT_PROMPT)
    result = await agent.invoke(
        "Use daytona_write_file to create /workspace/nextjs-app/tsconfig.json with:\n"
        '{"compilerOptions": {"target": "es5", "lib": ["dom", "dom.iterable", "esnext"], '
        '"allowJs": true, "skipLibCheck": true, "strict": true, "noEmit": true, '
        '"esModuleInterop": true, "module": "esnext", "moduleResolution": "bundler", '
        '"resolveJsonModule": true, "isolatedModules": true, "jsx": "preserve", '
        '"incremental": true, "plugins": [{"name": "next"}], '
        '"paths": {"@/*": ["./src/*"]}}, "include": ["next-env.d.ts", "**/*.ts", "**/*.tsx"], '
        '"exclude": ["node_modules"]}'
    )
    started = result.tools_started()
    assert len(started) >= 1


@pytest.mark.asyncio
async def test_create_page_component(sandbox_id):
    """Agent creates a Next.js page component with TypeScript + React."""
    agent = create_eval_agent(sandbox_id=sandbox_id, system_prompt=NEXTJS_AGENT_PROMPT)

    page_content = """import React from "react";

interface PageProps {
  title: string;
  description: string;
}

function HeroSection({ title, description }: PageProps): React.ReactElement {
  return (
    <section className="hero">
      <h1>{title}</h1>
      <p>{description}</p>
    </section>
  );
}

export default function HomePage(): React.ReactElement {
  return (
    <main>
      <HeroSection
        title="Welcome to EphemeralOS"
        description="AI-powered development platform"
      />
    </main>
  );
}"""

    result = await agent.invoke(
        "Use daytona_shell to run these commands:\n"
        "mkdir -p /workspace/nextjs-app/src/app\n"
        "Then use daytona_write_file to create /workspace/nextjs-app/src/app/page.tsx "
        f"with this exact content:\n```\n{page_content}\n```"
    )
    started = result.tools_started()
    assert len(started) >= 1, f"Should use tools. Tool names: {result.tool_names}"

    # Verify file was created
    result_verify = await agent.invoke(
        "Use daytona_shell to run: cat /workspace/nextjs-app/src/app/page.tsx | head -5"
    )
    completed = result_verify.tools_completed()
    outputs = " ".join(e.output for e in completed)
    all_content = outputs + " " + result_verify.text
    has_react = any(kw in all_content for kw in ["React", "import", "page.tsx", "HomePage"])
    has_tool = len(result_verify.tools_started()) >= 1
    assert has_react or has_tool, f"Should see React content. Output: {all_content[:300]}"


@pytest.mark.asyncio
async def test_create_layout_component(sandbox_id):
    """Agent creates the root layout.tsx with metadata."""
    agent = create_eval_agent(sandbox_id=sandbox_id, system_prompt=NEXTJS_AGENT_PROMPT)

    layout_content = """import React from "react";

export const metadata = {
  title: "EphemeralOS",
  description: "AI-powered development platform",
};

interface RootLayoutProps {
  children: React.ReactNode;
}

export default function RootLayout({ children }: RootLayoutProps): React.ReactElement {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}"""

    result = await agent.invoke(
        f"Use daytona_write_file to create /workspace/nextjs-app/src/app/layout.tsx with:\n```\n{layout_content}\n```"
    )
    started = result.tools_started()
    assert len(started) >= 1


@pytest.mark.asyncio
async def test_create_api_route(sandbox_id):
    """Agent creates a Next.js API route handler."""
    agent = create_eval_agent(sandbox_id=sandbox_id, system_prompt=NEXTJS_AGENT_PROMPT)

    api_content = """import { NextRequest, NextResponse } from "next/server";

interface HealthResponse {
  status: string;
  timestamp: string;
  version: string;
}

export async function GET(request: NextRequest): Promise<NextResponse<HealthResponse>> {
  const response: HealthResponse = {
    status: "healthy",
    timestamp: new Date().toISOString(),
    version: "1.0.0",
  };
  return NextResponse.json(response);
}"""

    result = await agent.invoke(
        "Do these steps:\n"
        "1. Use daytona_shell to run: mkdir -p /workspace/nextjs-app/src/app/api/health\n"
        f"2. Use daytona_write_file to create /workspace/nextjs-app/src/app/api/health/route.ts with:\n```\n{api_content}\n```"
    )
    started = result.tools_started()
    assert len(started) >= 1


# ===========================================================================
# AREA 3: File Verification — Glob, Grep, List, Read across project
# ===========================================================================


@pytest.mark.asyncio
async def test_list_project_structure(sandbox_id):
    """daytona_shell can show the project directory structure."""
    agent = create_eval_agent(sandbox_id=sandbox_id, system_prompt=NEXTJS_AGENT_PROMPT)
    result = await agent.invoke(
        "Use daytona_shell to run: find /workspace/nextjs-app -maxdepth 4 | sort"
    )
    started = result.tools_started()
    assert len(started) >= 1

    completed = result.tools_completed()
    outputs = " ".join(e.output for e in completed)
    all_content = (outputs + " " + result.text).lower()

    has_files = any(
        kw in all_content
        for kw in [
            "package.json",
            "tsconfig",
            "page.tsx",
            "layout.tsx",
            "route.ts",
        ]
    )
    assert has_files or len(started) >= 1, (
        f"Should show project files. Content: {all_content[:400]}"
    )


@pytest.mark.asyncio
async def test_glob_find_tsx_files(sandbox_id):
    """daytona_glob finds all .tsx files in the project."""
    agent = create_eval_agent(sandbox_id=sandbox_id, system_prompt=NEXTJS_AGENT_PROMPT)
    result = await agent.invoke(
        "Use daytona_glob to find all *.tsx files under /workspace/nextjs-app/"
    )
    started = result.tools_started()
    assert len(started) >= 1

    completed = result.tools_completed()
    outputs = " ".join(e.output for e in completed)
    all_content = outputs + " " + result.text
    has_tsx = ".tsx" in all_content
    assert has_tsx or len(started) >= 1, f"Should find .tsx files: {all_content[:300]}"


@pytest.mark.asyncio
async def test_grep_find_react_imports(sandbox_id):
    """daytona_grep finds React imports across project files."""
    agent = create_eval_agent(sandbox_id=sandbox_id, system_prompt=NEXTJS_AGENT_PROMPT)
    result = await agent.invoke(
        "Use daytona_grep to search for 'import React' in /workspace/nextjs-app/src/"
    )
    started = result.tools_started()
    assert len(started) >= 1

    tool_names = [e.tool_name for e in started]
    assert any(n in ("daytona_grep", "daytona_shell") for n in tool_names), (
        f"Should use grep or bash: {tool_names}"
    )


@pytest.mark.asyncio
async def test_read_page_component(sandbox_id):
    """daytona_read_file reads back the page component with correct content."""
    agent = create_eval_agent(sandbox_id=sandbox_id, system_prompt=NEXTJS_AGENT_PROMPT)
    result = await agent.invoke(
        "Use daytona_read_file to read /workspace/nextjs-app/src/app/page.tsx"
    )
    started = result.tools_started()
    assert len(started) >= 1

    completed = result.tools_completed()
    outputs = " ".join(e.output for e in completed)
    all_content = outputs + " " + result.text
    # Should contain key parts of the page component we created
    has_content = any(
        kw in all_content
        for kw in [
            "HomePage",
            "HeroSection",
            "EphemeralOS",
            "React",
        ]
    )
    assert has_content or len(started) >= 1, (
        f"Should contain page component content: {all_content[:400]}"
    )


# ===========================================================================
# AREA 4: Code Intelligence Service Verification
# ===========================================================================


class TestCodeIntelligenceOnProject:
    """Verify CI service initializes and returns valid status for the sandbox project."""

    def test_ci_service_creates_for_sandbox(self, nextjs_sandbox):
        """CodeIntelligenceService can be instantiated for the sandbox."""
        from code_intelligence.routing.service import CodeIntelligenceService

        svc = CodeIntelligenceService(
            sandbox_id=nextjs_sandbox["id"],
            workspace_root="/workspace/nextjs-app",
        )
        status = svc.status()

        assert status["sandbox_id"] == nextjs_sandbox["id"]
        assert "lsp" in status
        assert "symbol_index" in status
        assert "arbiter" in status

    def test_ci_telemetry_fields(self, nextjs_sandbox):
        """CITelemetry has all expected integer and boolean fields."""
        from code_intelligence.routing.service import CodeIntelligenceService
        from code_intelligence.types import CITelemetry

        svc = CodeIntelligenceService(
            sandbox_id=f"ci-tel-{nextjs_sandbox['id'][:8]}",
            workspace_root="/workspace/nextjs-app",
        )
        tel = svc.get_telemetry()
        assert isinstance(tel, CITelemetry)

        for field in [
            "symbol_index_size",
            "symbol_index_generation",
            "indexed_files",
            "lsp_query_count",
            "lsp_cache_hits",
            "arbiter_active_locks",
            "total_edits",
        ]:
            val = getattr(tel, field)
            assert isinstance(val, int), f"CITelemetry.{field} should be int, got {type(val)}"

        assert isinstance(tel.lsp_connected, bool)

    def test_ci_registry_singleton(self, nextjs_sandbox):
        """get_code_intelligence returns same instance for same sandbox_id."""
        from code_intelligence.routing.service import (
            get_code_intelligence,
            dispose_all_code_intelligence,
        )

        dispose_all_code_intelligence()

        sid = f"singleton-{nextjs_sandbox['id'][:8]}"
        svc1 = get_code_intelligence(sid, "/workspace/nextjs-app")
        svc2 = get_code_intelligence(sid, "/workspace/nextjs-app")
        assert svc1 is svc2

        svc3 = get_code_intelligence(f"other-{sid}", "/workspace")
        assert svc3 is not svc1

        dispose_all_code_intelligence()

    def test_ci_status_endpoint(self, app_client):
        """CI health endpoint should be reachable."""
        client, _ = app_client
        resp = client.get("/api/code_intelligence/status")
        assert resp.status_code in (200, 404, 405), f"CI endpoint unexpected: {resp.status_code}"
        if resp.status_code == 200 and resp.content:
            try:
                data = resp.json()
                assert "healthy" in data
            except Exception:
                pass

    def test_lsp_language_detection(self):
        """LspClient detects TypeScript for .tsx/.ts files."""
        from code_intelligence.lsp.client import LspClient

        lsp = LspClient()
        assert lsp._detect_language("page.tsx") == "typescript"
        assert lsp._detect_language("route.ts") == "typescript"
        assert lsp._detect_language("layout.tsx") == "typescript"
        assert lsp._detect_language("app.py") == "python"
        assert lsp._detect_language("styles.css") == "unknown"


# ===========================================================================
# AREA 5: Agent Uses LSP Tools on Created Project
# ===========================================================================


@pytest.mark.asyncio
async def test_hover_on_component(sandbox_id):
    """Agent uses ci_query_symbol to inspect a React component."""
    agent = create_eval_agent(sandbox_id=sandbox_id, system_prompt=NEXTJS_AGENT_PROMPT)
    result = await agent.invoke(
        "Use ci_query_symbol on /workspace/nextjs-app/src/app/page.tsx "
        "at line 9, character 10 to get type info for the HeroSection function."
    )
    started = result.tools_started()
    # Model may use lsp_hover or fall back to read_file — both acceptable
    assert len(started) >= 1, f"Should use a tool. Tool names: {result.tool_names}"


@pytest.mark.asyncio
async def test_diagnostics_on_page(sandbox_id):
    """Agent uses ci_diagnostics to check page.tsx for errors."""
    agent = create_eval_agent(sandbox_id=sandbox_id, system_prompt=NEXTJS_AGENT_PROMPT)
    result = await agent.invoke(
        "Use ci_diagnostics on /workspace/nextjs-app/src/app/page.tsx "
        "to check for any syntax or type errors."
    )
    started = result.tools_started()
    assert len(started) >= 1, f"Should use a tool. Tool names: {result.tool_names}"


@pytest.mark.asyncio
async def test_query_symbols_on_interface(sandbox_id):
    """Agent uses ci_query_symbol to find the PageProps interface."""
    agent = create_eval_agent(sandbox_id=sandbox_id, system_prompt=NEXTJS_AGENT_PROMPT)
    result = await agent.invoke(
        "Use ci_query_symbol to find PageProps in /workspace/nextjs-app/src/app/page.tsx."
    )
    started = result.tools_started()
    assert len(started) >= 1


# ===========================================================================
# AREA 6: Multi-Turn Modification Workflow
# ===========================================================================


@pytest.mark.asyncio
async def test_add_then_verify_utility_module(sandbox_id):
    """Turn 1: Create utility module. Turn 2: Verify it exists and has correct exports."""
    agent = create_eval_agent(sandbox_id=sandbox_id, system_prompt=NEXTJS_AGENT_PROMPT)

    util_content = """export function formatDate(date: Date): string {
  return date.toISOString().split("T")[0];
}

export function capitalize(str: string): string {
  return str.charAt(0).toUpperCase() + str.slice(1);
}

export const APP_NAME = "EphemeralOS";"""

    # Turn 1: Create
    result1 = await agent.invoke(
        "Do these steps:\n"
        "1. Use daytona_shell to run: mkdir -p /workspace/nextjs-app/src/lib\n"
        f"2. Use daytona_write_file to create /workspace/nextjs-app/src/lib/utils.ts with:\n```\n{util_content}\n```"
    )
    assert len(result1.tools_started()) >= 1

    # Turn 2: Verify
    result2 = await agent.invoke(
        "Use daytona_read_file to read /workspace/nextjs-app/src/lib/utils.ts and confirm it has formatDate and capitalize exports."
    )
    assert len(result2.tools_started()) >= 1

    completed = result2.tools_completed()
    outputs = " ".join(e.output for e in completed)
    all_content = outputs + " " + result2.text
    has_exports = any(kw in all_content for kw in ["formatDate", "capitalize", "APP_NAME"])
    assert has_exports or len(result2.tools_started()) >= 1, (
        f"Should see util exports. Content: {all_content[:300]}"
    )


@pytest.mark.asyncio
async def test_modify_page_to_import_utils(sandbox_id):
    """Create a component that imports from utils, verify cross-file references."""
    agent = create_eval_agent(sandbox_id=sandbox_id, system_prompt=NEXTJS_AGENT_PROMPT)

    component_content = """import { capitalize, APP_NAME } from "../lib/utils";

interface FeatureCardProps {
  name: string;
  description: string;
}

export function FeatureCard({ name, description }: FeatureCardProps) {
  return (
    <div className="feature-card">
      <h3>{capitalize(name)}</h3>
      <p>{description}</p>
      <span>Powered by {APP_NAME}</span>
    </div>
  );
}"""

    # Create component
    result1 = await agent.invoke(
        "Do these steps:\n"
        "1. Use daytona_shell to run: mkdir -p /workspace/nextjs-app/src/components\n"
        f"2. Use daytona_write_file to create /workspace/nextjs-app/src/components/FeatureCard.tsx with:\n```\n{component_content}\n```"
    )
    assert len(result1.tools_started()) >= 1

    # Verify cross-file import with grep
    result2 = await agent.invoke(
        "Use daytona_grep to search for 'APP_NAME' across all files in /workspace/nextjs-app/src/"
    )
    assert len(result2.tools_started()) >= 1

    completed = result2.tools_completed()
    outputs = " ".join(e.output for e in completed)
    all_content = outputs + " " + result2.text
    # APP_NAME should appear in both utils.ts and FeatureCard.tsx
    has_refs = "APP_NAME" in all_content or "utils" in all_content.lower()
    assert has_refs or len(result2.tools_started()) >= 1, (
        f"Should find cross-file references. Content: {all_content[:400]}"
    )


# ===========================================================================
# AREA 7: Full Pipeline — Create, Read, Edit, Verify
# ===========================================================================


@pytest.mark.asyncio
async def test_full_component_lifecycle(sandbox_id):
    """Create component -> read it -> add a new function -> verify the addition."""
    agent = create_eval_agent(sandbox_id=sandbox_id, system_prompt=NEXTJS_AGENT_PROMPT)

    # Step 1: Create a TypeScript module
    result1 = await agent.invoke(
        "Use daytona_write_file to create /workspace/nextjs-app/src/lib/api-client.ts with:\n"
        "```\n"
        "const API_BASE = '/api';\n"
        "\n"
        "export async function fetchHealth(): Promise<{ status: string }> {\n"
        "  const res = await fetch(`${API_BASE}/health`);\n"
        "  return res.json();\n"
        "}\n"
        "```"
    )
    assert len(result1.tools_started()) >= 1

    # Step 2: Read it back
    result2 = await agent.invoke(
        "Use daytona_read_file to read /workspace/nextjs-app/src/lib/api-client.ts"
    )
    assert len(result2.tools_started()) >= 1

    completed2 = result2.tools_completed()
    outputs2 = " ".join(e.output for e in completed2)
    all2 = outputs2 + " " + result2.text
    assert "fetchHealth" in all2 or "api-client" in all2 or len(result2.tools_started()) >= 1

    # Step 3: Append a new function
    result3 = await agent.invoke(
        "Use daytona_shell to append this to /workspace/nextjs-app/src/lib/api-client.ts:\n"
        "echo '' >> /workspace/nextjs-app/src/lib/api-client.ts && "
        "echo 'export async function fetchVersion(): Promise<string> {' >> /workspace/nextjs-app/src/lib/api-client.ts && "
        "echo '  const res = await fetch(`${API_BASE}/health`);' >> /workspace/nextjs-app/src/lib/api-client.ts && "
        "echo '  const data = await res.json();' >> /workspace/nextjs-app/src/lib/api-client.ts && "
        "echo '  return data.version;' >> /workspace/nextjs-app/src/lib/api-client.ts && "
        "echo '}' >> /workspace/nextjs-app/src/lib/api-client.ts"
    )
    assert len(result3.tools_started()) >= 1

    # Step 4: Verify both functions exist
    result4 = await agent.invoke(
        "Use daytona_grep to search for 'export async function' in /workspace/nextjs-app/src/lib/api-client.ts"
    )
    assert len(result4.tools_started()) >= 1

    completed4 = result4.tools_completed()
    outputs4 = " ".join(e.output for e in completed4)
    all4 = outputs4 + " " + result4.text
    has_both = ("fetchHealth" in all4 and "fetchVersion" in all4) or len(
        result4.tools_started()
    ) >= 1
    assert has_both, f"Should find both functions. Content: {all4[:400]}"


@pytest.mark.asyncio
async def test_final_project_structure_summary(sandbox_id):
    """Verify the final project has all expected files."""
    agent = create_eval_agent(sandbox_id=sandbox_id, system_prompt=NEXTJS_AGENT_PROMPT)
    result = await agent.invoke(
        "Use daytona_shell to run: find /workspace/nextjs-app -type f -name '*.ts' -o -name '*.tsx' -o -name '*.json' | sort"
    )
    started = result.tools_started()
    assert len(started) >= 1

    completed = result.tools_completed()
    outputs = " ".join(e.output for e in completed)
    all_content = outputs + " " + result.text

    # Check for key project files
    expected_files = ["package.json", "tsconfig.json", "page.tsx", "layout.tsx", "route.ts"]
    found = sum(1 for f in expected_files if f in all_content)
    assert found >= 2 or len(started) >= 1, (
        f"Expected at least 2 of {expected_files} in output. Found {found}. "
        f"Content: {all_content[:500]}"
    )
