"""Debug: run one OverlayExec call, print everything we can see."""

from __future__ import annotations

import asyncio
import sys
from pathlib import Path

BACKEND_SRC = Path(__file__).resolve().parents[1] / "backend" / "src"
sys.path.insert(0, str(BACKEND_SRC))

from code_intelligence.routing.overlay_exec import OverlayExec  # noqa: E402
from code_intelligence.routing.overlay_probe import (  # noqa: E402
    probe_overlay_capability,
)
from sandbox.testing import (  # noqa: E402
    create_test_sandbox,
    delete_test_sandbox,
    get_sandbox_service,
)


async def _exec(sandbox, command, *, timeout=None):
    if timeout is not None:
        return await sandbox.process.exec(command, timeout=timeout)
    return await sandbox.process.exec(command)


class _AsyncWrap:
    def __init__(self, raw):
        self._raw = raw

    class _Proc:
        def __init__(self, raw):
            self._raw = raw
        async def exec(self, command, timeout=None):
            if timeout is not None:
                return self._raw.process.exec(command, timeout=timeout)
            return self._raw.process.exec(command)

    @property
    def process(self):
        return self._Proc(self._raw)


async def main() -> int:
    info = create_test_sandbox(name="overlay-dbg2")
    sid = info["id"] if isinstance(info, dict) else info.id
    print(f"sandbox={sid}")
    try:
        svc = get_sandbox_service()
        raw = svc.get_sandbox_object(sid)
        async_sb = _AsyncWrap(raw)

        home = raw.process.exec("pwd", timeout=10).result.strip()
        repo = f"{home}/debug_repo"
        lower = f"/tmp/debug_lower_{sid[:6]}"

        import shlex
        setup_script = (
            f"set -ex; rm -rf {shlex.quote(repo)} {shlex.quote(lower)}; "
            f"mkdir -p {shlex.quote(repo)}/pkg && "
            f"printf 'x=1\\n' > {shlex.quote(repo)}/pkg/a.py && "
            f"cd {shlex.quote(repo)} && git init -q && "
            f"git -c user.email=t -c user.name=t add -A && "
            f"git -c user.email=t -c user.name=t commit -qm seed && "
            f"git -C {shlex.quote(repo)} worktree add --detach {shlex.quote(lower)} HEAD && "
            f"ls -la {shlex.quote(lower)}"
        )
        setup = raw.process.exec(f"bash -c {shlex.quote(setup_script)}", timeout=60)
        print(f"setup exit_code={getattr(setup, 'exit_code', '?')}")
        print(f"setup stdout:\n{setup.result}")

        probe = await probe_overlay_capability(async_sb, _exec)
        print(f"probe: supported={probe.supported} reason={probe.reason}")

        overlay = OverlayExec(exec_process=_exec, tmpfs_size="256m")
        user_cmd = (
            f"set -x; "
            f"pwd; "
            f"mkdir -p {repo}/pkg/generated && "
            f"printf 'v=1\\n' > {repo}/pkg/generated/new.py && "
            f"ls -la {repo}/pkg/generated; "
            f"echo DONE"
        )
        result = await overlay.execute(
            async_sb,
            user_command=user_cmd,
            lowerdir=lower,
            repo_root=repo,
            timeout=60,
        )
        print(f"exit_code={result.exit_code}")
        print(f"stdout:\n{result.stdout}")
        print(f"tar path: {result.audit_tar_path}")

        probe = raw.process.exec(
            f"ls -la {result.audit_tar_path} 2>&1; "
            f"echo '---'; "
            f"tar -tvf {result.audit_tar_path} 2>&1 | head -30; "
            f"echo '---mount_err---'; "
            f"cat {result.run_dir}/mount_err 2>&1",
            timeout=30,
        )
        print(probe.result)

    finally:
        try:
            delete_test_sandbox(sid)
        except Exception as exc:
            print(f"cleanup warn: {exc}")
    return 0


if __name__ == "__main__":
    raise SystemExit(asyncio.run(main()))
