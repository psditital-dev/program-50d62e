# PumpCuts router

Solana program (Pinocchio) that lets a pump.fun coin creator sell a share of the coin's future creator fees for a fixed
term, split into units. Buyers pay the creator up front; during the term every booked fee is split between the
platform, the unit holders and the creator; after the term every fee goes to the creator.

Program id: `D9bLR4M4jTVWhUgP9gFi4ZbfDnysvCXaJkuG8CeSqCSo`

## How it works

- Each coin has one receiver account (`["recv", mint]`). It is set as the 100% shareholder of the coin's pump.fun fee
  sharing config, so pump sends the coin's creator fees to it. pump allows this change once, so the receiver stays the
  coin's fee recipient for good; outside a term it forwards everything to the creator.
- A term: share of every fee (5-50%), number of units (10-10,000), fixed price per unit, 7 or 30 days from listing.
- Income is booked on `collect`, `buy`, `list_term` and `end_term_early`: income = receiver lamports - rent - debts.
  Income booked while `start <= now < end` is split: 2% to the platform, the sold share of the rest to all units of
  the term (per-unit index, scale 1e12), the rest (including the share of unsold units) to the creator. Income booked
  outside a term goes to the creator.
- Holders claim at any time, also after the term. Creator and platform shares are paid by `settle` (anyone can call).
- `buy` refuses while fees still wait in pump's own vaults, so a purchase never shares income earned before it.

## Accounts

1-byte discriminator at offset 0, bump at 1, little-endian.

| Account | Disc | Seeds | Len |
| --- | --- | --- | --- |
| Config | 1 | `["config"]` | 71 |
| Source (coin receiver) | 2 | `["recv", mint]` | 182 |
| Term | 3 | `["term", mint, seq u64 LE]` | 148 |
| Position | 4 | `["pos", term, owner]` | 134 |
| Source after `close_source` | 5 | same as Source | 98 |

Offsets: Config admin@2, platform@34, plat_bps@66 u16, fill_bps@68 u16, paused@70 u8. Source mint@2, creator@34,
seq@66, end@74, term@82, owed@114, creator_due@122, platform_due@130, total@138, payout@146, open_terms@178.
Term mint@2, seq@34, start@42, end@50, share@58, plat_bps@60, fill_bps@62, units@64, sold@68, price@72, index@80 u128,
acc@96 u128, f@112, plat_cum@120, reserved@128, claimed@136, open_positions@144. Position term@2, owner@34, units@66,
entry@70 u128, accrued@86, claimed@94, payout@102.

## Instructions (tag byte, then little-endian args)

| Tag | Name | Args |
| --- | --- | --- |
| 0 | init_config (upgrade authority; starts paused) | admin, platform |
| 1 | set_config (admin; a new admin co-signs) | admin, platform, fill_bps u16 (<= 200), paused u8 |
| 2 | register_source (coin creator) | mint |
| 3 | list_term (coin creator) | share u16, units u32, days u8, price u64 |
| 4 | buy | units u32 |
| 5 | collect (anyone) | - |
| 6 | settle (anyone) | 0 = creator, 1 = platform |
| 7 | claim (position owner) | - |
| 8 | refund_position (admin, after the term) | - |
| 9 | close_term (after the term, no open positions) | - |
| 10 | set_payout (creator for the coin, owner for a position) | payout |
| 11 | end_term_early (admin, paused) | - |
| 12 | close_source (admin, paused, no debts) | - |

Errors (`Custom(n)`): 1 Unauthorized, 2 Paused, 3 BadParams, 4 BadPumpAccount, 5 BadShares, 6 MathOverflow,
7 TermActive, 8 TermEnded, 9 TermNotEnded, 10 SoldOut, 11 NothingToClaim, 12 PositionsOpen, 13 BadAccount,
14 Insolvent, 15 FeesNotSwept, 16 PayoutBelowRent.

## Checks and limits

- `register_source`: SOL pair only; mayhem, cashback and holder-reward coins excluded; the signer must be the coin's
  creator (`bonding_curve.creator`, or `Pool.coin_creator` of the canonical pump pool after migration).
- `list_term` and `buy` re-check the pump route every time (our mint, version 2, active, admin revoked, this receiver
  as the only shareholder at 10000 bps). pump's global admin can re-open a sharing config; then sales stop while
  collect, claim, settle and refunds keep working.
- The creator's own key cannot buy units of its own coin.
- Payout addresses that can never be credited are refused when set; a low balance never redirects anyone's money.

## Pause and wind-down

Pause blocks registering, listing and buying only; collect, claim, settle and refunds keep working. Wind-down: pause,
collect every coin, `end_term_early`, `refund_position` for every position, `close_term`, `settle`, `close_source`
(the receiver shrinks to a forwarding stub that keeps sending the coin's fees to the creator).

## Build

```
solana-verify build --library-name pumpcuts_router
```
