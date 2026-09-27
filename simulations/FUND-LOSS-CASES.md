# Fund-loss cases

Sources: Lightning disaster-recovery docs (static channel backup, data-loss
protection), LDK's stale-manager and stale-monitor rules, and the restore
runs in this branch. A case is "wired" when this tree can actually lose or
strand funds if the check is missing.

## Already run

| Case | Backend | Result |
| --- | --- | --- |
| Fresh node starts | fs | pass, refusal does not block it |
| Kill -9, restart, channel still listed | fs | pass |
| Manager missing, monitors left behind | fs | refuses startup |
| Corrupt monitor, manager present | fs | process exits, API down |
| Kill -9, restart, channel ready | vss | pass |
| Pay 50_000 msat after that restart | vss | Success, preimage ff177419ae9e835f |
| VSS store wiped, local init marker kept | vss | refuses startup |

## Still to run

These are the public fund-loss cases that apply here and have not been
executed on this branch.

1. Stale manager, monitors ahead. Restoring an older `manager` while the
   monitors moved forward. LDK must force-close from the monitor. Funds
   stay claimable. The channel must not stay open. `recover.sh` R10.
2. Stale monitor, manager ahead. The dangerous direction. Startup must
   fail. Running would let the node sign a revoked state. `recover.sh` R14.
3. Peer force-closes while we are down. On restart the monitor must notice,
   the channel must end closed, and `SpendableOutputs` must still be
   claimable. LND's lost-backup reports are this case.
4. Kill during a VSS write. Restart must load the last committed monitor,
   not a torn one, and a later pay must still settle.
5. Two processes, one store. The second start must fail the pid lock or
   the VSS version fence. Two writers is split-brain and can publish a
   revoked commitment.
6. Static backup is not a hot backup. Copying `manager` to another machine
   and starting both is the penalty-transaction case. This node has no
   SCB. A copied store must not be started beside the live one.
7. Data-loss protection. After a real state loss, the peer should force-close
   and we should sweep with the seed. We must not broadcast an old
   commitment ourselves.
8. Unconfirmed funding restart. Kill after `fundchannel` returns and before
   the funding tx confirms. Restart must not drop the funding tx or open a
   second channel for the same coins.
9. Anchor / sweep delay. A force-close output must remain sweepable across
   restart. Dropping `SpendableOutputs` strands the on-chain balance even
   though the channel closed cleanly.
10. Clock skew on the VSS signature. The server rejects a signature more
    than 24 hours from now. A wrong clock must fail the request, not fall
    back to an empty store.

`recover.sh` already encodes 1, 2, and 3 for the filesystem store. It has
not been run on this branch, and it does not speak VSS.

## Added from public reports

11. Second process, same data dir. The pid lock refuses it:
    `impossible take a lock on the lampod.pid file`. Ran on the host.
12. Copied store started beside the original. The copy booted. There is no
    check that this data dir is already live elsewhere. That is the
    penalty-transaction case from the static-backup docs. Do not start a
    copied `manager` while the original node is up.
