# Generated public fixtures

`../fixture.py` constructs the repository deterministically using synthetic text and a reserved `.invalid` author address. No source repository is copied.

Mixed state includes staged and unstaged edits to the same unrelated path, a staged rename, an unstaged target edit, an unstaged deletion and an untracked filename with a space. Commit timestamps and author metadata are fixed. The isolated lifecycle starts from the same mixed main repository, but its new worktree begins at committed main and receives its own target edit.

Temporary repositories and provider envelopes belong in ignored `../.scratch/`; only reviewed allowlisted event projections and aggregate metrics belong in `../results/`.
