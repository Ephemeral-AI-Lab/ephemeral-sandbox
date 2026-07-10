"""Live e2e: File Blame — Sessionless (6 cases)."""

import pytest

from runtime.file.helpers import (
    assert_blame_ranges,
    assert_blame_tiling,
    assert_content,
    assert_error,
    assert_ok,
    assert_single_owner,
    edit,
    file_blame,
    file_edit,
    file_read,
    file_write,
    owners_by_line,
    sandbox_from_workspace,
)


def test_blame_key_resolution_and_unaudited_paths(tmp_path):
    """Blame key resolution and unaudited paths."""
    with sandbox_from_workspace(
        tmp_path,
        files={"base.txt": "base\nfile\n"},
    ) as sandbox:
        assert_ok(file_write(sandbox, "src/notes.txt", "one\ntwo\nthree"))

        direct = assert_blame_tiling(sandbox, "src/notes.txt")
        dotted = assert_ok(file_blame(sandbox, "./src/notes.txt"))
        owner = direct["ranges"][0]["owner"]
        assert owner.startswith("operation:")
        assert dotted["ranges"] == direct["ranges"]
        assert_blame_ranges(sandbox, "src/notes.txt", [(1, 3, owner)], direct)

        base_read = assert_ok(file_read(sandbox, "base.txt"))
        assert base_read["path"] == "base.txt"
        assert base_read["total_lines"] == 2
        assert_error(
            file_blame(sandbox, "base.txt"),
            "not_found",
            "no auditability record for path: base.txt",
        )
        assert_error(
            file_blame(sandbox, "src"),
            "not_found",
            "no auditability record for path: src",
        )
        assert_error(file_blame(sandbox, "src/../src/notes.txt"), "not_found")
        assert dotted == direct


def test_same_owner_coalescing_for_adjacent_lines_rewritten_in_one_edit(sandbox):
    """Same-owner coalescing."""
    path = "blame/coalesce.txt"
    seed = "\n".join(f"line-{index}" for index in range(1, 7))
    assert_ok(file_write(sandbox, path, seed))
    owner_a = assert_single_owner(sandbox, path, prefix="operation:")

    result = file_edit(
        sandbox,
        path,
        [
            edit("line-3", "LINE-3"),
            edit("line-4", "LINE-4"),
        ],
    )
    assert result["edits_applied"] == 2
    blame = assert_blame_tiling(sandbox, path)
    owner_b = owners_by_line(blame)[2]
    assert owner_b.startswith("operation:")
    assert owner_b != owner_a
    assert_blame_ranges(
        sandbox,
        path,
        [(1, 2, owner_a), (3, 2, owner_b), (5, 2, owner_a)],
        blame,
    )


def test_line_shift_on_insert_and_delete_preserves_untouched_owners(sandbox):
    """Line-shift on insert and delete."""
    path = "blame/shift.txt"
    seed = "\n".join(f"line-{index}" for index in range(1, 7))
    assert_ok(file_write(sandbox, path, seed))
    owner_a = assert_single_owner(sandbox, path, prefix="operation:")

    assert_ok(
        file_edit(
            sandbox,
            path,
            [edit("line-3", "insert-a\ninsert-b\ninsert-c")],
        )
    )
    after_insert = assert_blame_tiling(sandbox, path)
    owner_b = owners_by_line(after_insert)[2]
    assert owner_b.startswith("operation:")
    assert owner_b != owner_a
    assert_blame_ranges(
        sandbox,
        path,
        [(1, 2, owner_a), (3, 3, owner_b), (6, 3, owner_a)],
        after_insert,
    )

    assert_ok(
        file_edit(
            sandbox,
            path,
            [edit("insert-a\ninsert-b\ninsert-c", "line-3-new")],
        )
    )
    after_delete = assert_blame_tiling(sandbox, path)
    owner_c = owners_by_line(after_delete)[2]
    assert owner_c.startswith("operation:")
    assert owner_c not in {owner_a, owner_b}
    assert_blame_ranges(
        sandbox,
        path,
        [(1, 2, owner_a), (3, 1, owner_c), (4, 3, owner_a)],
        after_delete,
    )


def test_replace_all_multi_site_attribution_keeps_non_adjacent_ranges(sandbox):
    """`replace_all` multi-site attribution."""
    path = "blame/replace-all.txt"
    lines = [
        "line-1",
        "token",
        "line-3",
        "line-4",
        "token",
        "line-6",
        "line-7",
        "token",
        "line-9",
    ]
    assert_ok(file_write(sandbox, path, "\n".join(lines)))
    owner_a = assert_single_owner(sandbox, path, prefix="operation:")

    result = file_edit(
        sandbox,
        path,
        [edit("token", "TOKEN", replace_all=True)],
    )
    assert result["replacements"] == 3
    blame = assert_blame_tiling(sandbox, path)
    owner_b = owners_by_line(blame)[1]
    assert owner_b.startswith("operation:")
    assert owner_b != owner_a
    assert_blame_ranges(
        sandbox,
        path,
        [
            (1, 1, owner_a),
            (2, 1, owner_b),
            (3, 2, owner_a),
            (5, 1, owner_b),
            (6, 2, owner_a),
            (8, 1, owner_b),
            (9, 1, owner_a),
        ],
        blame,
    )


@pytest.mark.slow
def test_complex_50_round_insert_delete_ladder_from_many_actors(sandbox):
    """[complex] 50-round insert/delete ladder from many actors."""
    path = "blame/ladder.txt"
    lines = [f"seed-{index}" for index in range(1, 11)]
    assert_ok(file_write(sandbox, path, "\n".join(lines)))
    seed_owner = assert_single_owner(sandbox, path, prefix="operation:")
    owners = [seed_owner] * len(lines)
    inserted_tokens = []

    for round_number in range(1, 51):
        if round_number % 2:
            token = f"inserted-{round_number}"
            old_string = f"{lines[0]}\n{lines[1]}"
            new_string = f"{lines[0]}\n{token}\n{lines[1]}"
            assert_ok(file_edit(sandbox, path, [edit(old_string, new_string)]))
            lines.insert(1, token)
            owner = owners_by_line(assert_blame_tiling(sandbox, path))[1]
            assert owner.startswith("operation:")
            owners.insert(1, owner)
            inserted_tokens.append(token)
        else:
            token = inserted_tokens.pop()
            index = lines.index(token)
            old_string = f"{token}\n" if index < len(lines) - 1 else f"\n{token}"
            assert_ok(file_edit(sandbox, path, [edit(old_string, "")]))
            lines.pop(index)
            owners.pop(index)

        if round_number % 10 == 0:
            assert_content(file_read(sandbox, path), "\n".join(lines))
            blame = assert_blame_tiling(sandbox, path)
            assert owners_by_line(blame) == owners
            assert "unknown" not in set(owners_by_line(blame))

    blame = assert_blame_tiling(sandbox, path)
    assert owners_by_line(blame) == owners
    assert set(owners) == {seed_owner}


@pytest.mark.slow
def test_complex_owner_turnover_across_30_wholesale_generations(sandbox):
    """[complex] Owner turnover across 30 wholesale generations."""
    path = "blame/turnover.txt"
    seen = set()
    counts = [5, 40, 12]

    for generation in range(1, 31):
        line_count = counts[(generation - 1) % len(counts)]
        content = "\n".join(
            f"generation-{generation}-line-{line}" for line in range(1, line_count + 1)
        )
        assert_ok(file_write(sandbox, path, content))
        blame = assert_blame_tiling(sandbox, path)
        assert len(blame["ranges"]) == 1
        owner = blame["ranges"][0]["owner"]
        assert owner.startswith("operation:")
        assert owner not in seen
        seen.add(owner)
        assert_blame_ranges(sandbox, path, [(1, line_count, owner)], blame)
