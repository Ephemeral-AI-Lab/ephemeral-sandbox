"""Live e2e coverage for exec_command git publish policy: complex cases."""

from __future__ import annotations

import pytest

from runtime.command.test_git_policy_easy import (
    GitCaseRecorder,
    assert_content,
    assert_git_clean,
    assert_git_operable,
    assert_ignored_blame,
    assert_not_found,
    assert_source_blame,
    assert_terminal_publish_rejection,
    axis_source,
    commit_cmd,
    exec_any,
    exec_ok,
    file_read,
    git_case,
    init_repo,
    init_repo_cmd,
    layerstack,
    read_content,
    release_gated_command,
    route_summary,
    seed_repo,
    sh,
    start_gated_command,
)
from runtime.file.helpers import assert_manifest_delta

pytestmark = [pytest.mark.git, pytest.mark.complex]


def test_CX_01_two_agents_commit_different_files_rejects_cleanly(tmp_path):
    with GitCaseRecorder("CX-01") as rec, git_case(tmp_path, rec) as sandbox:
        seed_repo(sandbox, rec, {"base.txt": "base\n"})
        before = layerstack(sandbox)
        agent_a = start_gated_command(
            sandbox,
            "printf 'A\\n' > a.txt\ngit add a.txt\n" + commit_cmd("A"),
            rec,
            name="agent-a-start",
        )
        agent_b = start_gated_command(
            sandbox,
            "printf 'B\\n' > b.txt\ngit add b.txt\n" + commit_cmd("B"),
            rec,
            name="agent-b-start",
        )
        release_gated_command(sandbox, agent_a, rec, name="agent-a-finalize")
        rejected = release_gated_command(
            sandbox,
            agent_b,
            rec,
            name="agent-b-finalize",
            allow_publish_reject=True,
        )
        assert_terminal_publish_rejection(rejected, "source_conflict")
        assert_manifest_delta(sandbox, before, 1)
        assert_content(sandbox, "a.txt", "A\n")
        assert_not_found(file_read(sandbox, "b.txt"))
        assert_source_blame(sandbox, "a.txt", expected_lines=1)
        assert_git_operable(sandbox, rec, name="post-disjoint-commits")

        rec.axis(
            "correctness",
            True,
            "A landed and stale B rejected source_conflict",
            route="source",
            manifest_delta=1,
            reject_class="source_conflict",
            route_summary=route_summary(source=1),
        )
        rec.axis("attribution", True, "a.txt present and source-blamed; b.txt did not leak")
        rec.axis("isolation", True, "fresh git fsck/status succeeded after discarded commit")


def test_CX_02_two_agents_commit_same_file_one_wins(tmp_path):
    with GitCaseRecorder("CX-02") as rec, git_case(tmp_path, rec) as sandbox:
        seed_repo(sandbox, rec, {"shared.txt": "base\n"})
        before = layerstack(sandbox)
        agent_a = start_gated_command(
            sandbox,
            "printf 'A\\n' > shared.txt\ngit add shared.txt\n" + commit_cmd("A"),
            rec,
            name="agent-a-start",
        )
        agent_b = start_gated_command(
            sandbox,
            "printf 'B\\n' > shared.txt\ngit add shared.txt\n" + commit_cmd("B"),
            rec,
            name="agent-b-start",
        )
        release_gated_command(sandbox, agent_a, rec, name="agent-a-finalize")
        rejected = release_gated_command(
            sandbox,
            agent_b,
            rec,
            name="agent-b-finalize",
            allow_publish_reject=True,
        )
        assert_terminal_publish_rejection(rejected, "source_conflict")
        assert_manifest_delta(sandbox, before, 1)
        assert_content(sandbox, "shared.txt", "A\n")
        assert_source_blame(sandbox, "shared.txt", expected_lines=1)
        assert_git_clean(sandbox, rec, name="post-same-file-status")

        rec.axis(
            "correctness",
            True,
            "same-file stale commit rejected source_conflict",
            route="source",
            manifest_delta=1,
            reject_class="source_conflict",
            route_summary=route_summary(source=1),
        )
        rec.axis("attribution", True, "shared.txt reflects only A")
        rec.axis("isolation", True, "fresh git status clean after B discard")


def test_CX_03_concurrent_refs_heads_main_do_not_interleave(tmp_path):
    with GitCaseRecorder("CX-03") as rec, git_case(tmp_path, rec) as sandbox:
        seed_repo(sandbox, rec, {"base.txt": "base\n"})
        before = layerstack(sandbox)
        agent_a = start_gated_command(
            sandbox,
            "printf 'A\\n' > a.txt\ngit add a.txt\n" + commit_cmd("A") + "\ngit rev-parse HEAD",
            rec,
            name="agent-a-start",
        )
        agent_b = start_gated_command(
            sandbox,
            "printf 'B\\n' > b.txt\ngit add b.txt\n" + commit_cmd("B") + "\ngit rev-parse HEAD",
            rec,
            name="agent-b-start",
        )
        landed = release_gated_command(sandbox, agent_a, rec, name="agent-a-finalize")
        rejected = release_gated_command(
            sandbox,
            agent_b,
            rec,
            name="agent-b-finalize",
            allow_publish_reject=True,
        )
        assert_terminal_publish_rejection(rejected, "source_conflict")
        assert_manifest_delta(sandbox, before, 1)
        ref = read_content(sandbox, ".git/refs/heads/main")
        assert ref == landed["output"].splitlines()[-1], (ref, landed)
        exec_ok(sandbox, "git -C /workspace --no-pager log --format=%s --max-count=1", rec, name="fresh-log")

        rec.axis(
            "correctness",
            True,
            "refs/heads/main kept A hash exactly and B rejected",
            route="source",
            manifest_delta=1,
            reject_class="source_conflict",
            route_summary=route_summary(source=1),
        )
        rec.axis("attribution", True, ".git/refs/heads/main equals A hash exactly")
        rec.axis("isolation", True, "fresh git log succeeded after ref conflict")


def test_CX_04_git_gc_churn_with_concurrent_commit_keeps_db_intact(tmp_path):
    with GitCaseRecorder("CX-04") as rec, git_case(tmp_path, rec) as sandbox:
        setup = [init_repo_cmd()]
        for index in range(1, 21):
            setup.extend(
                [
                    f"printf '{index}\\n' > file-{index}.txt",
                    "git add -A",
                    commit_cmd(f"c{index}"),
                ]
            )
        exec_ok(sandbox, sh("\n".join(setup)), rec, name="setup-20-commits", timeout=720)
        before = layerstack(sandbox)
        agent_a = start_gated_command(sandbox, "git gc -q", rec, name="agent-a-gc-start")
        agent_b = start_gated_command(
            sandbox,
            "printf 'new\\n' > concurrent.txt\ngit add concurrent.txt\n" + commit_cmd("concurrent"),
            rec,
            name="agent-b-commit-start",
        )
        first = release_gated_command(sandbox, agent_a, rec, name="agent-a-gc-finalize", allow_publish_reject=True)
        second = release_gated_command(
            sandbox,
            agent_b,
            rec,
            name="agent-b-commit-finalize",
            allow_publish_reject=True,
        )
        accepted = int(first.get("publish_rejected") is not True) + int(second.get("publish_rejected") is not True)
        assert accepted in {1, 2}, (first, second)
        assert_manifest_delta(sandbox, before, accepted)
        assert_git_operable(sandbox, rec, name="post-gc")
        exec_ok(sandbox, "git -C /workspace cat-file -p HEAD >/dev/null", rec, name="cat-head")

        rec.axis(
            "correctness",
            True,
            "git gc and commit landed-or-cleanly-rejected",
            route="source",
            manifest_delta=accepted,
            route_summary=route_summary(source=accepted),
        )
        rec.axis("attribution", True, "HEAD is readable after pack churn")
        rec.axis("isolation", True, "git fsck --full/status succeeded after gc race")


def test_CX_05_large_repo_import_is_unforbidden(tmp_path):
    with GitCaseRecorder("CX-05") as rec, git_case(tmp_path, rec) as sandbox:
        before = layerstack(sandbox)
        script = [
            "rm -rf /tmp/git-policy-src /workspace/repo",
            "mkdir -p /tmp/git-policy-src",
            "cd /tmp/git-policy-src",
            init_repo_cmd(),
        ]
        for index in range(1, 81):
            script.extend(
                [
                    f"printf '{index}\\n' > f-{index}.txt",
                    "git add -A",
                    commit_cmd(f"c{index}"),
                ]
            )
        script.append("git clone -q /tmp/git-policy-src /workspace/repo")
        exec_ok(
            sandbox,
            "set -eu\n" + "\n".join(script),
            rec,
            name="large-import",
            timeout_ms=900_000,
            timeout=960,
        )
        assert_manifest_delta(sandbox, before, 1)
        count = exec_ok(
            sandbox,
            "git -C /workspace/repo --no-pager log --oneline | wc -l",
            rec,
            name="import-log-count",
        )
        assert int(count["output"].strip()) == 80, count

        axis_source(rec, "large imported repo published without protected or opaque-dir rejection", manifest_delta=1)
        rec.axis("attribution", True, "fresh git log count matched imported history length")


def test_CX_06_stale_destructive_delete_rejects_settled_delete_allowed(tmp_path):
    with GitCaseRecorder("CX-06") as rec, git_case(tmp_path, rec) as sandbox:
        seed_repo(sandbox, rec, {"victim.txt": "base\n", "settled.txt": "settled\n"})
        before = layerstack(sandbox)
        agent_a = start_gated_command(
            sandbox,
            "printf 'A\\n' > victim.txt\ngit add victim.txt\n" + commit_cmd("A"),
            rec,
            name="agent-a-start",
        )
        agent_b = start_gated_command(
            sandbox,
            "git rm -q victim.txt\n" + commit_cmd("delete-victim"),
            rec,
            name="agent-b-start",
        )
        release_gated_command(sandbox, agent_a, rec, name="agent-a-finalize")
        rejected = release_gated_command(
            sandbox,
            agent_b,
            rec,
            name="agent-b-finalize",
            allow_publish_reject=True,
        )
        assert_terminal_publish_rejection(rejected, "source_conflict")
        assert_manifest_delta(sandbox, before, 1)
        assert_content(sandbox, "victim.txt", "A\n")

        settled_before = layerstack(sandbox)
        exec_ok(
            sandbox,
            sh("git rm -q settled.txt\n" + commit_cmd("delete-settled")),
            rec,
            name="settled-delete",
        )
        assert_manifest_delta(sandbox, settled_before, 1)
        assert_not_found(file_read(sandbox, "settled.txt"))

        rec.axis(
            "correctness",
            True,
            "stale clobber rejected while settled delete landed",
            route="source",
            manifest_delta=2,
            reject_class="source_conflict",
            route_summary=route_summary(source=2),
        )
        rec.axis("attribution", True, "victim retained A content; settled path deleted")
        rec.axis("isolation", True, "OCC caught only concurrent clobber")


def test_CX_07_routing_uses_base_gitignore_not_racing_one(tmp_path):
    with GitCaseRecorder("CX-07") as rec, git_case(tmp_path, rec) as sandbox:
        init_repo(sandbox, rec)
        before = layerstack(sandbox)
        agent_b = start_gated_command(sandbox, "printf 'x\\ny\\n' > data.bin", rec, name="agent-b-start")
        exec_ok(sandbox, sh("printf 'data.bin\\n' > .gitignore"), rec, name="agent-a-gitignore")
        release_gated_command(sandbox, agent_b, rec, name="agent-b-finalize")
        assert_manifest_delta(sandbox, before, 2)
        assert_content(sandbox, "data.bin", "x\ny\n")
        assert_source_blame(sandbox, "data.bin", expected_lines=2)

        rec.axis(
            "correctness",
            True,
            "B routed data.bin from its base without racing .gitignore",
            route="source",
            manifest_delta=2,
            route_summary=route_summary(source=2),
        )
        rec.axis("attribution", True, "data.bin blame was per-line source, not wholesale ignored")
        rec.axis("isolation", True, "base-pinned ignore oracle held under race")


def test_CX_08_symlink_under_repo_routes_as_source(tmp_path):
    with GitCaseRecorder("CX-08") as rec, git_case(tmp_path, rec) as sandbox:
        before = layerstack(sandbox)
        exec_ok(
            sandbox,
            sh(
                "\n".join(
                    [
                        init_repo_cmd(),
                        "printf 'target-data\\n' > target",
                        "ln -s target link",
                        "git add -A",
                        commit_cmd("link"),
                    ]
                )
            ),
            rec,
            name="symlink-commit",
        )
        assert_manifest_delta(sandbox, before, 1)
        link = exec_ok(sandbox, "test -L /workspace/link && readlink /workspace/link", rec, name="readlink")
        assert link["output"] == "target", link
        resolved = exec_ok(sandbox, "cat /workspace/link", rec, name="cat-link")
        assert resolved["output"] == "target-data", resolved
        assert_content(sandbox, "target", "target-data\n")
        assert_source_blame(sandbox, "target", expected_lines=1)

        axis_source(rec, "symlink commit published as source", manifest_delta=1)
        rec.axis("attribution", True, "link exists as symlink; target content and source blame match")


def test_CX_09_interleaved_git_soak_keeps_invariants(tmp_path):
    with GitCaseRecorder("CX-09") as rec, git_case(tmp_path, rec) as sandbox:
        seed_repo(sandbox, rec, {".gitignore": "ignored.log\n", "tracked.txt": "base\n"})
        suite_before = layerstack(sandbox)
        accepted_total = 0
        rejected_total = 0
        for index in range(1, 16):
            iter_before = layerstack(sandbox)
            if index % 3 == 1:
                agent_a = start_gated_command(
                    sandbox,
                    f"printf 'new-{index}\\n' > new-{index}.txt\ngit add new-{index}.txt\n" + commit_cmd(f"new-{index}"),
                    rec,
                    name=f"iter-{index}-a-start",
                )
                agent_b = start_gated_command(
                    sandbox,
                    f"printf 'ignored-{index}\\n' > ignored.log",
                    rec,
                    name=f"iter-{index}-b-start",
                )
                release_gated_command(sandbox, agent_a, rec, name=f"iter-{index}-a-finalize")
                release_gated_command(sandbox, agent_b, rec, name=f"iter-{index}-b-finalize")
                accepted = 2
            elif index % 3 == 2:
                agent_a = start_gated_command(
                    sandbox,
                    f"printf 'A-{index}\\n' > tracked.txt\ngit add tracked.txt\n" + commit_cmd(f"A-{index}"),
                    rec,
                    name=f"iter-{index}-a-start",
                )
                agent_b = start_gated_command(
                    sandbox,
                    f"printf 'B-{index}\\n' > tracked.txt\ngit add tracked.txt\n" + commit_cmd(f"B-{index}"),
                    rec,
                    name=f"iter-{index}-b-start",
                )
                release_gated_command(sandbox, agent_a, rec, name=f"iter-{index}-a-finalize")
                rejected = release_gated_command(
                    sandbox,
                    agent_b,
                    rec,
                    name=f"iter-{index}-b-finalize",
                    allow_publish_reject=True,
                )
                assert_terminal_publish_rejection(rejected, "source_conflict")
                accepted = 1
                rejected_total += 1
            else:
                exec_ok(
                    sandbox,
                    sh(f"printf 'solo-{index}\\n' > solo-{index}.txt\ngit add solo-{index}.txt\n" + commit_cmd(f"solo-{index}")),
                    rec,
                    name=f"iter-{index}-solo",
                )
                accepted = 1
            accepted_total += accepted
            assert_manifest_delta(sandbox, iter_before, accepted)
            assert_git_operable(sandbox, rec, name=f"iter-{index}-git")
            assert_source_blame(sandbox, "tracked.txt", expected_lines=1)
            if index % 3 == 1:
                assert_ignored_blame(sandbox, "ignored.log")

        assert_manifest_delta(sandbox, suite_before, accepted_total)
        rec.axis(
            "correctness",
            True,
            "15-iteration deterministic soak preserved publish invariants",
            route="mixed",
            manifest_delta=accepted_total,
            rejects=rejected_total,
            route_summary=route_summary(source=accepted_total, ignored=5),
        )
        rec.axis("attribution", True, "sampled source blame tiled and ignored blame stayed wholesale")
        rec.axis("isolation", True, "git fsck/status succeeded after every iteration")


def test_CX_10_destructive_policy_is_caller_prehook_not_layerstack(tmp_path):
    with GitCaseRecorder("CX-10") as rec, git_case(tmp_path, rec) as sandbox:
        seed_repo(sandbox, rec, {"safe.txt": "base\n"})
        before_commit = layerstack(sandbox)
        exec_ok(
            sandbox,
            sh("printf 'benign\\n' > safe.txt\ngit add safe.txt\n" + commit_cmd("benign")),
            rec,
            name="benign-commit",
        )
        assert_manifest_delta(sandbox, before_commit, 1)

        before_block = layerstack(sandbox)
        blocked = exec_any(
            sandbox,
            "set -eu\ncmd='git reset --hard'\ncase \"$cmd\" in *'reset --hard'*) echo prehook-blocked; exit 42;; esac\n$cmd",
            rec,
            name="prehook-block",
            allow_error_status=True,
        )
        assert blocked["exit_code"] == 42, blocked
        assert blocked["output"] == "prehook-blocked", blocked
        assert blocked.get("publish_rejected") is not True, blocked
        after_block = layerstack(sandbox)
        assert after_block["manifest_version"] == before_block["manifest_version"], after_block
        assert after_block["root_hash"] == before_block["root_hash"], after_block
        assert_content(sandbox, "safe.txt", "benign\n")

        rec.axis(
            "correctness",
            True,
            "benign commit landed; destructive command blocked before capture",
            route="source",
            manifest_delta=1,
            prehook_exit_code=42,
            route_summary=route_summary(source=1),
        )
        rec.axis("attribution", True, "merged tree reflects only the benign commit")
        rec.axis("isolation", True, "no layerstack publish_reject_class was involved in prehook block")
