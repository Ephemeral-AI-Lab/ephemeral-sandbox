"""Live e2e coverage for exec_command git publish policy: medium cases."""

from __future__ import annotations

import pytest

from runtime.command.test_git_policy_easy import (
    GitCaseRecorder,
    assert_content,
    assert_exec_workspace_not_found,
    assert_git_operable,
    assert_ignored_blame,
    assert_layer_ids_unchanged,
    assert_not_found,
    assert_source_blame,
    assert_terminal_publish_rejection,
    axis_ignored,
    axis_rejected,
    axis_source,
    commit_cmd,
    exec_any,
    exec_command,
    exec_ok,
    file_blame,
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
from runtime.file.correctness.test_correctness_sessionless import _assert_stack_unchanged
from runtime.file.helpers import assert_manifest_delta

pytestmark = [pytest.mark.git, pytest.mark.medium]


def test_MED_01_first_writer_wins_on_tracked_file(tmp_path):
    with GitCaseRecorder("MED-01") as rec, git_case(tmp_path, rec) as sandbox:
        seed_repo(sandbox, rec, {"README.md": "base\n"})
        before = layerstack(sandbox)
        agent_a = start_gated_command(
            sandbox,
            "printf 'A\\n' > README.md\ngit add README.md\n" + commit_cmd("A"),
            rec,
            name="agent-a-start",
        )
        agent_b = start_gated_command(
            sandbox,
            "printf 'B\\n' > README.md\ngit add README.md\n" + commit_cmd("B"),
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
        assert_content(sandbox, "README.md", "A\n")
        owners = assert_source_blame(sandbox, "README.md", expected_lines=1)
        assert owners[0] == agent_a["workspace_session_id"] or owners[0].startswith("workspace_session:")
        assert_git_operable(sandbox, rec, name="post-conflict")

        rec.axis(
            "correctness",
            True,
            "A landed and stale B rejected source_conflict",
            route="source",
            manifest_delta=1,
            reject_class="source_conflict",
            route_summary=route_summary(source=1),
        )
        rec.axis("attribution", True, "README content and blame reflect only A")
        rec.axis("isolation", True, "fresh git fsck and status succeeded after B discard")


def test_MED_02_last_writer_wins_on_gitignored_path(tmp_path):
    with GitCaseRecorder("MED-02") as rec, git_case(tmp_path, rec) as sandbox:
        seed_repo(sandbox, rec, {".gitignore": "cache.bin\n"})
        before = layerstack(sandbox)
        agent_a = start_gated_command(sandbox, "printf 'alpha\\n' > cache.bin", rec, name="agent-a-start")
        agent_b = start_gated_command(sandbox, "printf 'beta\\n' > cache.bin", rec, name="agent-b-start")
        release_gated_command(sandbox, agent_a, rec, name="agent-a-finalize")
        release_gated_command(sandbox, agent_b, rec, name="agent-b-finalize")
        assert_manifest_delta(sandbox, before, 2)
        assert_content(sandbox, "cache.bin", "beta\n")
        owner = assert_ignored_blame(sandbox, "cache.bin")
        assert owner.startswith("workspace_session:"), owner

        axis_ignored(rec, "both ignored writes landed and B won", manifest_delta=2)
        rec.axis("attribution", True, "cache.bin byte content is beta and blame is wholesale")
        rec.axis("isolation", True, "ignored route produced no source_conflict")


def test_MED_03_git_reset_hard_revert_is_not_forbidden(tmp_path):
    with GitCaseRecorder("MED-03") as rec, git_case(tmp_path, rec) as sandbox:
        seed_repo(sandbox, rec, {"keep.txt": "committed\n"})
        before = layerstack(sandbox)
        exec_ok(
            sandbox,
            sh("printf 'dirty\\n' >> keep.txt\ngit checkout -- keep.txt"),
            rec,
            name="checkout-revert",
        )
        after = layerstack(sandbox)
        assert after["manifest_version"] in {before["manifest_version"], before["manifest_version"] + 1}, after
        assert_content(sandbox, "keep.txt", "committed\n")

        rec.axis(
            "correctness",
            True,
            "destructive checkout was not layerstack-forbidden",
            route="source",
            manifest_delta=after["manifest_version"] - before["manifest_version"],
            route_summary=route_summary(source=1),
        )
        rec.axis("attribution", True, "keep.txt returned to committed byte content")


def test_MED_04_git_clean_publishes_untracked_deletions(tmp_path):
    with GitCaseRecorder("MED-04") as rec, git_case(tmp_path, rec) as sandbox:
        seed_repo(sandbox, rec, {"tracked.txt": "tracked\n"})
        exec_ok(
            sandbox,
            sh("mkdir -p scratch\nprintf 'a\\n' > scratch/a\nprintf 'b\\n' > scratch/b"),
            rec,
            name="write-untracked-scratch",
        )
        before = layerstack(sandbox)
        exec_ok(sandbox, sh("git clean -fdq"), rec, name="git-clean")
        assert_manifest_delta(sandbox, before, 1)
        assert_not_found(file_read(sandbox, "scratch/a"))
        assert_not_found(file_read(sandbox, "scratch/b"))

        axis_source(rec, "git clean deletions published", manifest_delta=1, source_count=2)
        rec.axis("attribution", True, "both untracked paths are not_found after clean")


def test_MED_05_binary_git_index_divergence_rejects_cleanly(tmp_path):
    with GitCaseRecorder("MED-05") as rec, git_case(tmp_path, rec) as sandbox:
        seed_repo(sandbox, rec, {"base.txt": "base\n"})
        before = layerstack(sandbox)
        agent_a = start_gated_command(
            sandbox,
            "printf 'A\\n' > a.txt\ngit add a.txt\nsha256sum .git/index | awk '{print $1}'",
            rec,
            name="agent-a-start",
        )
        agent_b = start_gated_command(
            sandbox,
            "printf 'B\\n' > b.txt\ngit add b.txt\nsha256sum .git/index | awk '{print $1}'",
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
        index_sha = exec_ok(
            sandbox,
            "sha256sum /workspace/.git/index | awk '{print $1}'",
            rec,
            name="index-sha-after",
        )
        assert index_sha["output"] == landed["output"].splitlines()[-1], (index_sha, landed)
        assert_git_operable(sandbox, rec, name="post-index-conflict")

        rec.axis(
            "correctness",
            True,
            "binary .git/index divergence rejected source_conflict",
            route="source",
            manifest_delta=1,
            reject_class="source_conflict",
            route_summary=route_summary(source=1),
        )
        rec.axis("attribution", True, "fresh .git/index sha equals A's published index")
        rec.axis("isolation", True, "git fsck/status clean after binary conflict")


def test_MED_06_text_git_log_divergence_merges_or_rejects(tmp_path):
    with GitCaseRecorder("MED-06") as rec, git_case(tmp_path, rec) as sandbox:
        seed_repo(sandbox, rec, {"f1.txt": "1\n"})
        exec_ok(
            sandbox,
            sh("printf '2\\n' > f2.txt\ngit add f2.txt\n" + commit_cmd("c2")),
            rec,
            name="second-commit",
        )
        before = layerstack(sandbox)
        append_a = (
            "h=$(git rev-parse HEAD)\n"
            "printf \"$h $h t <t@e> 1783050000 +0000\\tgit-policy-A\\n\" >> .git/logs/HEAD"
        )
        append_b = (
            "h=$(git rev-parse HEAD)\n"
            "printf \"$h $h t <t@e> 1783050001 +0000\\tgit-policy-B\\n\" >> .git/logs/HEAD"
        )
        agent_a = start_gated_command(sandbox, append_a, rec, name="agent-a-start")
        agent_b = start_gated_command(sandbox, append_b, rec, name="agent-b-start")
        release_gated_command(sandbox, agent_a, rec, name="agent-a-finalize")
        result_b = release_gated_command(
            sandbox,
            agent_b,
            rec,
            name="agent-b-finalize",
            allow_publish_reject=True,
        )
        content = read_content(sandbox, ".git/logs/HEAD")
        if result_b.get("publish_rejected"):
            assert result_b.get("publish_reject_class") == "source_conflict", result_b
            assert "git-policy-A" in content and "git-policy-B" not in content, content
            delta = 1
        else:
            assert "git-policy-A" in content and "git-policy-B" in content, content
            delta = 2
        assert_manifest_delta(sandbox, before, delta)
        exec_ok(sandbox, "git -C /workspace --no-pager log --oneline --max-count=1", rec, name="git-log-ok")

        rec.axis(
            "correctness",
            True,
            "text .git log append either merged or rejected source_conflict",
            route="source",
            manifest_delta=delta,
            reject_class=result_b.get("publish_reject_class"),
            route_summary=route_summary(source=1),
        )
        rec.axis("attribution", True, ".git/logs/HEAD content was merge-or-A-only")
        rec.axis("isolation", True, "fresh git log succeeded")


def test_MED_07_gitignored_dotgit_is_last_writer_wins(tmp_path):
    with GitCaseRecorder("MED-07") as rec, git_case(tmp_path, rec) as sandbox:
        exec_ok(sandbox, sh("printf '.git/\\n' > .gitignore"), rec, name="publish-gitignore-dotgit")
        before = layerstack(sandbox)
        agent_a = start_gated_command(
            sandbox,
            "\n".join([init_repo_cmd(), "printf 'alpha\\n' > .git/git-policy-marker", "printf 'A\\n' > a.txt"]),
            rec,
            name="agent-a-start",
        )
        agent_b = start_gated_command(
            sandbox,
            "\n".join([init_repo_cmd(), "printf 'beta\\n' > .git/git-policy-marker", "printf 'B\\n' > b.txt"]),
            rec,
            name="agent-b-start",
        )
        release_gated_command(sandbox, agent_a, rec, name="agent-a-finalize")
        release_gated_command(sandbox, agent_b, rec, name="agent-b-finalize")
        assert_manifest_delta(sandbox, before, 2)
        assert_content(sandbox, ".git/git-policy-marker", "beta\n")
        assert_ignored_blame(sandbox, ".git/git-policy-marker")

        rec.axis(
            "correctness",
            True,
            ".git/ gitignore made .git internals ignored and B won",
            route="ignored",
            manifest_delta=2,
            route_summary=route_summary(source=2, ignored=1),
        )
        rec.axis("attribution", True, ".git marker content came from B with wholesale blame")
        rec.axis("isolation", True, "no source_conflict on ignored .git internals")


def test_MED_08_mixed_source_ignored_changeset_rejects_atomically(tmp_path):
    with GitCaseRecorder("MED-08") as rec, git_case(tmp_path, rec) as sandbox:
        seed_repo(sandbox, rec, {".gitignore": "out.log\n", "src.txt": "base\n"})
        before = layerstack(sandbox)
        agent_b = start_gated_command(
            sandbox,
            "printf 'B\\n' > src.txt\nprintf 'ignored\\n' > out.log\ngit add src.txt\n" + commit_cmd("B"),
            rec,
            name="agent-b-start",
        )
        exec_ok(
            sandbox,
            sh("printf 'A\\n' > src.txt\ngit add src.txt\n" + commit_cmd("A")),
            rec,
            name="agent-a-advance",
        )
        rejected = release_gated_command(
            sandbox,
            agent_b,
            rec,
            name="agent-b-finalize",
            allow_publish_reject=True,
        )
        assert_terminal_publish_rejection(rejected, "source_conflict")
        assert_manifest_delta(sandbox, before, 1)
        assert_content(sandbox, "src.txt", "A\n")
        assert_not_found(file_read(sandbox, "out.log"))

        rec.axis(
            "correctness",
            True,
            "source conflict rejected mixed changeset atomically",
            route="mixed",
            manifest_delta=1,
            reject_class="source_conflict",
            route_summary=route_summary(source=1, ignored=1),
        )
        rec.axis("attribution", True, "out.log did not leak from rejected mixed publish")
        rec.axis("isolation", True, "A's src.txt survived B discard")


def test_MED_09_branch_switch_rewrites_worktree(tmp_path):
    with GitCaseRecorder("MED-09") as rec, git_case(tmp_path, rec) as sandbox:
        exec_ok(
            sandbox,
            sh(
                "\n".join(
                    [
                        init_repo_cmd(),
                        "printf 'main\\n' > only_main.txt",
                        "git add -A",
                        commit_cmd("main"),
                        "git checkout -q -b feat",
                        "rm only_main.txt",
                        "printf 'feat\\n' > only_feat.txt",
                        "git add -A",
                        commit_cmd("feat"),
                        "git checkout -q main",
                    ]
                )
            ),
            rec,
            name="setup-branches",
        )
        before = layerstack(sandbox)
        exec_ok(sandbox, sh("git checkout -q feat"), rec, name="checkout-feat")
        assert_manifest_delta(sandbox, before, 1)
        assert_content(sandbox, "only_feat.txt", "feat\n")
        assert_not_found(file_read(sandbox, "only_main.txt"))
        assert_content(sandbox, ".git/HEAD", "ref: refs/heads/feat\n")

        axis_source(rec, "branch checkout worktree rewrite published", manifest_delta=1, source_count=3)
        rec.axis("attribution", True, "feat file present, main-only file removed, HEAD points to feat")


def test_MED_10_protected_layer_metadata_rejects_and_discards(tmp_path):
    with GitCaseRecorder("MED-10") as rec, git_case(tmp_path, rec) as sandbox:
        init_repo(sandbox, rec)
        before = layerstack(sandbox)
        result = exec_ok(
            sandbox,
            sh("mkdir -p .layer-metadata\nprintf 'x\\n' > .layer-metadata/state"),
            rec,
            name="protected-layer-metadata",
            allow_publish_reject=True,
        )
        assert_terminal_publish_rejection(result, "protected_path")
        session = result["workspace_session_id"]
        assert_exec_workspace_not_found(
            exec_command(sandbox, "true", workspace_session_id=session),
            session,
        )
        _assert_stack_unchanged(sandbox, before)
        assert_layer_ids_unchanged(sandbox, before)
        assert_not_found(file_read(sandbox, ".layer-metadata/state"))

        axis_rejected(rec, ".layer-metadata publish rejected and discarded", "protected_path")
        rec.axis("attribution", True, "n/a: protected changeset was discarded whole")
