# Release channels: stable and experimental

botsu is published as two Docker images:

| Image | Branch | For |
|---|---|---|
| `caaarrrlll/botsu:latest` | `master` | everyday use; what the live servers run |
| `caaarrrlll/botsu:experimental` | `experimental` | trying bigger or riskier changes before they reach `latest` |

Both are built and published by CI on every push to their branch (docs-only pushes skip the
image). Version tags (`vX.Y.Z`) are made from `master` only.

## Which channel a change goes to

Decide per change, by how much it can break and how hard it is to undo:

| Change | Goes to | Examples |
|---|---|---|
| **Small or low-risk** | `master`, then merged into `experimental` (both channels) | wording, help text, docs, a bug fix confined to one command, a new setting that's off by default, dependency patch updates |
| **Big or risky** | `experimental` first; merged into `master` after it has run live without problems | new features that post, delete or change things on servers; database migrations; changes to the adapters, voice, alert delivery or startup; dependency major updates |

When unsure, use `experimental`. A database migration always goes to `experimental` first: an
older `latest` can't read a database an `experimental` build has already migrated, so switching
back after that needs the data folder's backup.

Workflow:
1. **Both channels:** commit to `master`, push, then `git checkout experimental`,
   `git merge master`, push, and go back to `master`.
2. **Experimental first:** commit to `experimental` and push. After the live test,
   `git checkout master`, `git merge experimental`, push.

`experimental` is always `master` plus changes not released yet; it never falls behind `master`.

## Running the experimental image

Run it with its **own bot account** (a second application in the Discord Developer Portal and on
Fluxer, with its own tokens and data folder) on a test server. Two containers using the same token
would both answer every command.

To move the main container over instead (Unraid: Docker → botsu → Edit → Repository
`caaarrrlll/botsu:experimental` → Apply), back up the data folder first (see the migration note
above) and switch back to `latest` the same way.
