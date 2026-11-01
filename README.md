# Sheda Collection Contracts

The contracts behind property raises on Sheda: a vault that owns one unit on
behalf of many people and splits its rent among them, and the stake registry
that backs a builder's promises.

This is stage 1 and 2 of [the build spec][spec]: custody, shares, income, and
the plug-in to the rental marketplace that already exists. The raise, the
builder guarantee and governance come next and are not here yet.

[spec]: https://claude.ai/artifact/9ttLG6SjqLLK5SV5dEtt47

---

## The idea in one paragraph

The Sheda rental marketplace knows one thing about ownership: a property NFT
has an owner account, and that owner lists it, accepts bids and gets paid. It
never asks whether that account belongs to a person. This contract is an
account. It holds the NFT, so it is the landlord, and **the marketplace needs
no change at all** to let a flat owned by three hundred people be rented
exactly like a flat owned by one.

What the vault adds is the split. Shares are a fungible token; rent arriving
is divided among whoever holds them, in proportion, in arrears.

## Why shares are fungible and not smaller NFTs

Ten people splitting a flat do not own ten distinguishable things. They own
ten identical claims, and the point is that those claims divide again, add
together, and sell in part. That is a fungible token.

Numbered sub-NFTs would mean writing division, merging and partial transfer by
hand — every one of them a place to get arithmetic wrong with other people's
money. So the unit stays an NFT, because a flat is a specific flat, and the
claims on it are fungible, because a tenth of the rent is a tenth of the rent.

## Nobody is paid when rent arrives

The obvious implementation loops over holders and pays each one. It stops
working past a few dozen of them, because a transaction has a gas ceiling and
the loop eventually does not fit inside it. It would pass every test and fail
once the product succeeded.

So one number moves instead:

```
on income:   acc_per_share += income × 1e18 ÷ total_shares
a holder:    owed = balance × acc_per_share ÷ 1e18 − already_credited
on claim:    pay owed, then already_credited = balance × acc_per_share ÷ 1e18
```

Crediting income costs the same for ten holders and ten thousand, because
nothing iterates over them. Holders pull with `claim()`.

**The rule that makes it honest:** a balance may not move until both sides are
settled against the accumulator. `internal_transfer` settles the sender and
the receiver, moves the shares, then re-baselines both. Without it, someone
buying shares on Monday would collect rent that accrued in March.

The division remainder is carried in `dust` and joins the next credit, so
repeated small payments lose nothing.

## Rent arrives silently

The deployed marketplace pays landlords with plain `ft_transfer`. This was
checked rather than assumed — reading the bytecode of
`market.shedauser.testnet` over RPC shows the literals:

```
accept_bid … "ft_transfer successful, proceeding with NFT transfer and bid updates"
cross-contract args … ft_transfer · receiver_id · amount · memo
```

There is no standalone `ft_transfer_call` anywhere in it, and NEP-141
`ft_transfer` notifies nobody. So money lands in the vault without the vault
being told, and `sync_income()` goes and looks: it reads the token balance,
compares it to what was last accounted for, and credits the difference.

It is open to anyone to call. If it needed the manager, a manager who stopped
calling it would freeze every holder out of their own rent.

Two details that matter:

- The balance read is compared against a **snapshot taken when the sync
  started**, not against `last_accounted` at callback time. A claim settling
  in between moves that figure, and using it would credit money that had
  already left.
- A share purchase must never arrive at this same balance, or it would be
  distributed as rent. The raise escrow is a separate account for exactly
  this reason.

## What is in here

A cargo workspace. Each contract deploys on its own account.

| Crate | Holds |
| --- | --- |
| `contracts/vault` | One unit, its shares, and its rent |
| `contracts/stake-registry` | Builder and manager stake, per profile |

Inside the vault:

| File | Holds |
| --- | --- |
| `lib.rs` | State, initialisation, governance, views |
| `shares.rs` | The share ledger and NEP-141 core, with settle-on-transfer |
| `income.rs` | Accumulator, `sync_income`, `claim`, fees, reserve |
| `custody.rs` | Taking the unit in, and redemption |
| `market.rs` | Manager instructions forwarded to the marketplace |
| `ext.rs` | The contracts this one talks to |
| `tests.rs` | The arithmetic that decides who gets paid what |

## The stake registry

Stake belongs to a profile, not to a deal, so a builder who abandons one
project finds it harder to raise for the next.

The accounting is the point. A builder with ₦10m staked who opens three
raises, each advertising 18% cover, has promised the same ₦10m three times —
and every investor sees a figure that is only true if the other two succeed.
So a balance is **total**, **allocated** and **free**, and a raise cannot
advertise cover the free balance does not support.

Each allocation is ring-fenced to its own project. A slash reaches only that
project's share, because otherwise one failure drains the cover out from
under investors in a different project who did nothing wrong.

Cover is reported from the contract as basis points of a target rather than
computed in the app, so the figure on a project card is the figure the chain
would actually pay.

### Methods

**Anyone**

- `sync_income()` — credit whatever has arrived
- `ft_balance_of`, `ft_total_supply`, `ft_metadata`, `ownership_bps`
- `claimable(account_id)` — settled plus accrued, right now
- `get_info()`, `get_reserve()`, `withdraw_treasury_fee()`

**Holders** (1 yocto)

- `ft_transfer(receiver_id, amount, memo)`
- `claim()`
- `redeem()` — only with every share, and only when the flat is not let

**Manager** (1 yocto)

- `list_for_rent(price, lease_duration_days)`, `delist()`
- `accept_bid(bid_id)`, `reject_bid(bid_id)`
- `release_documents(bid_id, uri, description)` — forwards to the marketplace's `confirm_document_release`
- `draw_reserve(amount, to, reason)`
- `withdraw_manager_fee()`

**Governance** (1 yocto)

- `mint_shares(to, amount)`, `freeze_supply()`
- `set_manager(manager)`, `set_fees(manager_bps, reserve_bps, platform_bps)`

## Rules the contract enforces

- Fees together may never exceed **30%** of rent. A vault where configuration
  could leave holders with nothing is one where it eventually will.
- Supply freezes permanently at the close of the raise. No mint path survives
  it, for anyone — dilution after people have bought is indistinguishable
  from theft.
- A unit is only accepted from its own marketplace, and only if its token id
  matches the property this vault was created for.
- Redemption takes **every** share, and is refused while the flat is let. A
  redemption mid-tenancy would hand the NFT to someone who was not party to
  the lease and leave the tenant's deposit against a vault owning nothing.
- A failed payout restores what it was paying. The one outcome worse than a
  payout not happening is a holder's entitlement being erased while the money
  stays in the vault.
- Replacing a manager does not cancel fees they have already earned.

## Testing

```bash
cargo test --workspace --lib
cargo clippy --all-targets -- -D warnings
cargo build --workspace --target wasm32-unknown-unknown --release
```

CI runs all three on every push. The unit tests cover the risk-register rows
that can be proven without a network: pro-rata distribution, settlement on
transfer in both directions, rounding conservation across a hundred
payments, the fee split reconstructing the whole, the fee ceiling, the supply
freeze, and a thousand-holder reconciliation.

What they do not cover is anything crossing contracts — `sync_income`,
`claim`, `redeem` and the marketplace forwarders all need a sandbox. Those
tests come with stage 2's integration gate, against a real deployed
marketplace.

## Not here yet

Raise and escrow, governance and voting, and the share market. They are stages 3 to 6 of the spec. This
contract is deliberately the piece that can be proven first, because the
integration claim it rests on is the one that would force a redesign if it
turned out to be wrong.

`ft_transfer_call` is also absent. Shares move by `ft_transfer` for now; the
share market will need the call variant and will bring its resolve path with
it rather than having it sit here unused.

## Licence

Unpublished. All rights reserved by Sheda Solutions.
