//! PumpCuts router: permanent per-mint pump.fun fee receiver that sells a share of the
//! fee income BOOKED by this program inside a fixed term. See ../README.md.
#![cfg_attr(not(test), no_std)]

pub mod math;

use math::{earned, mul_div, Book, BPS};
use pinocchio::cpi::{Seed, Signer};
use pinocchio::error::ProgramError;
use pinocchio::sysvars::{clock::Clock, rent::Rent, Sysvar};
use pinocchio::{AccountView, Address, ProgramResult, Resize};
use pinocchio_system::instructions::{Allocate, Assign, Transfer};

#[cfg(not(feature = "no-entrypoint"))]
pinocchio::program_entrypoint!(process_instruction, 13);
pinocchio::no_allocator!();
pinocchio::nostd_panic_handler!();

// Dev id (sha256 "pumpcuts-router-dev-id"); the launch kit replaces it with the deploy keypair's address.
pub const ID: Address = Address::from_str_const("D9bLR4M4jTVWhUgP9gFi4ZbfDnysvCXaJkuG8CeSqCSo");
const SYSTEM: Address = pinocchio_system::ID;
const LOADER: Address = Address::from_str_const("BPFLoaderUpgradeab1e11111111111111111111111");
const PUMP: Address = Address::from_str_const("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P");
const PUMP_FEES: Address = Address::from_str_const("pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ");
const AMM: Address = Address::from_str_const("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA");
const TOKEN: Address = Address::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
const ATA: Address = Address::from_str_const("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
const WSOL: Address = Address::from_str_const("So11111111111111111111111111111111111111112");
const SYSVAR: Address = Address::from_str_const("Sysvar1111111111111111111111111111111111111");
const ZERO: Address = Address::new_from_array([0; 32]);
// Runtime-reserved ids that are never writable yet are neither executable nor sysvar-owned.
const RESERVED: [Address; 5] = [
    Address::from_str_const("StakeConfig11111111111111111111111111111111"),
    Address::from_str_const("LoaderV411111111111111111111111111111111111"),
    Address::from_str_const("ZkTokenProof1111111111111111111111111111111"),
    Address::from_str_const("NativeLoader1111111111111111111111111111111"),
    SYSVAR,
];
const BC_DISC: [u8; 8] = [23, 183, 248, 55, 96, 216, 172, 96];
const SC_DISC: [u8; 8] = [216, 74, 9, 0, 56, 140, 93, 75];
const POOL_DISC: [u8; 8] = [241, 154, 109, 4, 17, 177, 109, 188];

const PLAT_BPS: u16 = 200; // platform share of every fee booked in a term
const MAX_FILL_BPS: u16 = 200;
const MIN_PRICE: u64 = 100_000;
// In-term income below this waits for the next collect; buy hands it to the creator instead.
// Also the most unswept pump fee a buy tolerates.
const MIN_COLLECT: u64 = 1_000;
const DAY: i64 = 86_400;

// One-byte account discriminators and fixed layouts (byte offsets, little-endian).
const CONFIG: u8 = 1;
const SOURCE: u8 = 2;
const TERM: u8 = 3;
const POSITION: u8 = 4;
const CLOSED: u8 = 5; // wound-down Source: forwards everything to the creator's payout
mod c {
    pub const BUMP: usize = 1;
    pub const ADMIN: usize = 2;
    pub const PLATFORM: usize = 34;
    pub const PLAT_BPS: usize = 66;
    pub const FILL_BPS: usize = 68;
    pub const PAUSED: usize = 70;
    pub const LEN: usize = 71;
}
mod s {
    pub const BUMP: usize = 1;
    pub const MINT: usize = 2;
    pub const CREATOR: usize = 34; // authority for list/set_payout, fixed at registration
    pub const SEQ: usize = 66; // u64, number of terms listed
    pub const END: usize = 74; // i64, end of the latest term
    pub const TERM: usize = 82; // latest term address
    pub const OWED: usize = 114; // u64, reserved for holders (all terms)
    pub const CREATOR_DUE: usize = 122;
    pub const PLATFORM_DUE: usize = 130;
    pub const TOTAL: usize = 138; // u64, lifetime income booked (saturating, stats only)
    pub const PAYOUT: usize = 146; // creator's payout address (unit sales + creator share)
    pub const OPEN_TERMS: usize = 178; // u32, listed and not yet closed terms
    pub const LEN: usize = 182;
}
// Compacted Source after close_source (same address, same pump recipient).
mod x {
    pub const BUMP: usize = 1;
    pub const MINT: usize = 2;
    pub const CREATOR: usize = 34;
    pub const PAYOUT: usize = 66;
    pub const LEN: usize = 98;
}
mod t {
    pub const BUMP: usize = 1;
    pub const MINT: usize = 2;
    pub const SEQ: usize = 34;
    pub const START: usize = 42;
    pub const END: usize = 50;
    pub const SHARE: usize = 58; // u16
    pub const PLAT: usize = 60; // u16, snapshot of config at listing
    pub const FILL: usize = 62; // u16, snapshot of config at listing
    pub const UNITS: usize = 64; // u32
    pub const SOLD: usize = 68; // u32
    pub const PRICE: usize = 72; // u64
    pub const INDEX: usize = 80; // u128
    pub const ACC: usize = 96; // u128
    pub const F: usize = 112; // u64
    pub const PLAT_CUM: usize = 120; // u64
    pub const RESERVED: usize = 128; // u64
    pub const CLAIMED: usize = 136; // u64
    pub const OPEN: usize = 144; // u32, open positions
    pub const LEN: usize = 148;
}
mod p {
    pub const BUMP: usize = 1;
    pub const TERM: usize = 2;
    pub const OWNER: usize = 34;
    pub const UNITS: usize = 66; // u32
    pub const ENTRY: usize = 70; // u128
    pub const ACCRUED: usize = 86; // u64
    pub const CLAIMED: usize = 94; // u64
    pub const PAYOUT: usize = 102; // holder's payout address
    pub const LEN: usize = 134;
}

#[repr(u32)]
enum Error {
    Unauthorized = 1,
    Paused,
    BadParams,
    BadPumpAccount,
    BadShares,
    MathOverflow,
    TermActive,
    TermEnded,
    TermNotEnded,
    SoldOut,
    NothingToClaim,
    PositionsOpen,
    BadAccount,
    Insolvent,
    FeesNotSwept,
    PayoutBelowRent,
}
fn err(e: Error) -> ProgramError {
    ProgramError::Custom(e as u32)
}
fn need(ok: bool, e: Error) -> ProgramResult {
    if ok {
        Ok(())
    } else {
        Err(err(e))
    }
}
fn rd<const N: usize>(d: &[u8], o: usize) -> Result<[u8; N], ProgramError> {
    let end = o.checked_add(N).ok_or(ProgramError::InvalidAccountData)?;
    d.get(o..end)
        .ok_or(ProgramError::InvalidAccountData)?
        .try_into()
        .map_err(|_| ProgramError::InvalidAccountData)
}
fn byte(d: &[u8], o: usize) -> Result<u8, ProgramError> {
    d.get(o).copied().ok_or(ProgramError::InvalidAccountData)
}
fn key(d: &[u8], o: usize) -> Result<Address, ProgramError> {
    Ok(Address::new_from_array(rd(d, o)?))
}
fn u16_at(d: &[u8], o: usize) -> Result<u16, ProgramError> {
    Ok(u16::from_le_bytes(rd(d, o)?))
}
fn u32_at(d: &[u8], o: usize) -> Result<u32, ProgramError> {
    Ok(u32::from_le_bytes(rd(d, o)?))
}
fn u64_at(d: &[u8], o: usize) -> Result<u64, ProgramError> {
    Ok(u64::from_le_bytes(rd(d, o)?))
}
/// Trailing field appended by a later layout: missing on old accounts reads as 0 (pump docs).
fn u64_or_zero(d: &[u8], o: usize) -> u64 {
    u64_at(d, o).unwrap_or(0)
}
fn i64_at(d: &[u8], o: usize) -> Result<i64, ProgramError> {
    Ok(i64::from_le_bytes(rd(d, o)?))
}
fn u128_at(d: &[u8], o: usize) -> Result<u128, ProgramError> {
    Ok(u128::from_le_bytes(rd(d, o)?))
}
fn put(d: &mut [u8], o: usize, v: &[u8]) -> ProgramResult {
    let end = o.checked_add(v.len()).ok_or(ProgramError::InvalidAccountData)?;
    d.get_mut(o..end)
        .ok_or(ProgramError::InvalidAccountData)?
        .copy_from_slice(v);
    Ok(())
}
fn add(a: u64, b: u64) -> Result<u64, ProgramError> {
    a.checked_add(b).ok_or(err(Error::MathOverflow))
}
fn sub(a: u64, b: u64) -> Result<u64, ProgramError> {
    a.checked_sub(b).ok_or(err(Error::MathOverflow))
}
fn inc(d: &mut [u8], o: usize, v: u64) -> ProgramResult {
    let x = add(u64_at(d, o)?, v)?;
    put(d, o, &x.to_le_bytes())
}
fn dec(d: &mut [u8], o: usize, v: u64) -> ProgramResult {
    let x = sub(u64_at(d, o)?, v)?;
    put(d, o, &x.to_le_bytes())
}
fn writable(a: &AccountView) -> ProgramResult {
    need(a.is_writable(), Error::BadAccount)
}
fn signer(a: &AccountView) -> ProgramResult {
    if a.is_signer() {
        Ok(())
    } else {
        Err(ProgramError::MissingRequiredSignature)
    }
}
fn system(a: &AccountView) -> ProgramResult {
    need(a.address() == &SYSTEM, Error::BadAccount)
}
fn pda_of(seeds: &[&[u8]], program: &Address) -> Address {
    Address::find_program_address(seeds, program).0
}
fn find(seeds: &[&[u8]], a: &AccountView) -> Result<u8, ProgramError> {
    let (address, bump) = Address::find_program_address(seeds, &ID);
    need(a.address() == &address, Error::BadAccount)?;
    Ok(bump)
}
fn check_pda(a: &AccountView, seeds: &[&[u8]]) -> ProgramResult {
    let address = Address::create_program_address(seeds, &ID).map_err(|_| err(Error::BadAccount))?;
    need(a.address() == &address, Error::BadAccount)
}
fn load<const N: usize>(a: &AccountView, disc: u8) -> Result<[u8; N], ProgramError> {
    if !a.owned_by(&ID) {
        return Err(ProgramError::IllegalOwner);
    }
    if a.data_len() != N {
        return Err(ProgramError::InvalidAccountData);
    }
    let d: [u8; N] = rd(&a.try_borrow()?, 0)?;
    need(d[0] == disc, Error::BadAccount)?;
    Ok(d)
}
fn config(a: &AccountView) -> Result<[u8; c::LEN], ProgramError> {
    let d = load(a, CONFIG)?;
    check_pda(a, &[b"config", &[d[c::BUMP]]])?;
    Ok(d)
}
fn source(a: &AccountView) -> Result<[u8; s::LEN], ProgramError> {
    let d = load(a, SOURCE)?;
    check_pda(a, &[b"recv", &d[s::MINT..s::MINT + 32], &[d[s::BUMP]]])?;
    Ok(d)
}
/// Term of this source, checked by seeds (mint + seq) and stored bump.
fn closed(a: &AccountView) -> Result<[u8; x::LEN], ProgramError> {
    let d = load(a, CLOSED)?;
    check_pda(a, &[b"recv", &d[x::MINT..x::MINT + 32], &[d[x::BUMP]]])?;
    Ok(d)
}
fn term(a: &AccountView, src: &[u8]) -> Result<[u8; t::LEN], ProgramError> {
    let d = load(a, TERM)?;
    need(d[t::MINT..t::MINT + 32] == src[s::MINT..s::MINT + 32], Error::BadAccount)?;
    check_pda(a, &[b"term", &d[t::MINT..t::MINT + 32], &d[t::SEQ..t::SEQ + 8], &[d[t::BUMP]]])?;
    Ok(d)
}
fn position(a: &AccountView) -> Result<[u8; p::LEN], ProgramError> {
    let d = load(a, POSITION)?;
    check_pda(a, &[b"pos", &d[p::TERM..p::TERM + 32], &d[p::OWNER..p::OWNER + 32], &[d[p::BUMP]]])?;
    Ok(d)
}
fn save(a: &mut AccountView, d: &[u8]) -> ProgramResult {
    writable(a)?;
    let mut data = a.try_borrow_mut()?;
    need(data.len() == d.len(), Error::BadAccount)?;
    data.copy_from_slice(d);
    Ok(())
}
fn live(cfg: &[u8]) -> ProgramResult {
    need(cfg[c::PAUSED] == 0, Error::Paused)
}
fn now() -> Result<i64, ProgramError> {
    Ok(Clock::get()?.unix_timestamp)
}
fn rent(len: usize) -> Result<u64, ProgramError> {
    Rent::get()?.try_minimum_balance(len)
}
fn init_account(payer: &AccountView, new: &AccountView, len: usize, seeds: &[Seed]) -> ProgramResult {
    signer(payer)?;
    writable(payer)?;
    writable(new)?;
    // Never overwrite records; a prefunded empty address is still usable (anti-griefing).
    if !new.owned_by(&SYSTEM) || new.data_len() != 0 {
        return Err(ProgramError::AccountAlreadyInitialized);
    }
    let shortfall = rent(len)?.saturating_sub(new.lamports());
    if shortfall > 0 {
        Transfer { from: payer, to: new, lamports: shortfall }.invoke()?;
    }
    let signers = [Signer::from(seeds)];
    Allocate { account: new, space: len as u64 }.invoke_signed(&signers)?;
    Assign { account: new, owner: &ID }.invoke_signed(&signers)
}
/// Move lamports out of an account this program owns.
fn pay(from: &mut AccountView, to: &mut AccountView, amount: u64) -> ProgramResult {
    writable(from)?;
    writable(to)?;
    need(from.address() != to.address(), Error::BadAccount)?;
    if amount == 0 {
        return Ok(());
    }
    let left = sub(from.lamports(), amount)?;
    let got = add(to.lamports(), amount)?;
    from.set_lamports(left);
    to.set_lamports(got);
    Ok(())
}
/// An address the runtime can never credit: executable, sysvar-owned, reserved id, or this
/// program's own state. Only such STRUCTURAL failures reroute money to the platform wallet;
/// a low or zero balance never does.
fn uncreditable(a: &AccountView) -> bool {
    a.executable() || a.owned_by(&SYSVAR) || a.owned_by(&ID) || RESERVED.contains(a.address())
}
/// Pay `amount` from the source to the stored payout (platform if uncreditable). `extra` = lamports
/// the same payout receives later in this instruction (closed position rent). An empty payout that
/// would end below the rent-exempt minimum is refused with PayoutBelowRent: the debt stays.
fn pay_out(
    src: &mut AccountView,
    dest: &mut AccountView,
    payout: &Address,
    platform: &mut AccountView,
    cd: &[u8],
    amount: u64,
    extra: u64,
) -> ProgramResult {
    need(dest.address() == payout, Error::Unauthorized)?;
    if uncreditable(dest) {
        need(platform.address() == &key(cd, c::PLATFORM)?, Error::Unauthorized)?;
        return pay(src, platform, amount);
    }
    need(
        amount == 0 || dest.lamports() > 0 || add(amount, extra)? >= rent(0)?,
        Error::PayoutBelowRent,
    )?;
    pay(src, dest, amount)
}
fn close(acc: &mut AccountView, to: &mut AccountView) -> ProgramResult {
    let all = acc.lamports();
    pay(acc, to, all)?;
    acc.try_borrow_mut()?.fill(0);
    acc.close()
}

/// Book everything on the receiver above rent + liabilities. Donations and direct pump
/// distributions count too: they only add money. The window is the BOOKING time: income booked
/// while start <= now < end is split (platform / holders via index / creator), anything booked
/// outside it is the creator's. `buy` passes `dust_to_creator` so a sub-threshold remainder that
/// arrived before the purchase is never shared with the new units.
fn book(
    src: &mut [u8; s::LEN],
    lamports: u64,
    term: Option<&mut [u8; t::LEN]>,
    now: i64,
    dust_to_creator: bool,
) -> ProgramResult {
    let owed = u64_at(src, s::OWED)?;
    let creator_due = u64_at(src, s::CREATOR_DUE)?;
    let platform_due = u64_at(src, s::PLATFORM_DUE)?;
    let held = add(add(rent(s::LEN)?, owed)?, add(creator_due, platform_due)?)?;
    let income = lamports.checked_sub(held).ok_or(err(Error::Insolvent))?;
    if income == 0 {
        return Ok(());
    }
    let in_term = now < i64_at(src, s::END)?;
    if in_term && income < MIN_COLLECT && !dust_to_creator {
        return Ok(()); // stays on the receiver for the next collect
    }
    let total = u64_at(src, s::TOTAL)?.saturating_add(income);
    put(src, s::TOTAL, &total.to_le_bytes())?;
    if !in_term || income < MIN_COLLECT {
        return put(src, s::CREATOR_DUE, &add(creator_due, income)?.to_le_bytes());
    }
    let tm = term.ok_or(err(Error::BadAccount))?;
    let mut b = Book {
        f: u64_at(tm, t::F)?,
        plat: u64_at(tm, t::PLAT_CUM)?,
        index: u128_at(tm, t::INDEX)?,
        acc: u128_at(tm, t::ACC)?,
        reserved: u64_at(tm, t::RESERVED)?,
    };
    let step = b
        .add(income, u16_at(tm, t::PLAT)?, u16_at(tm, t::SHARE)?, u32_at(tm, t::UNITS)?, u32_at(tm, t::SOLD)?)
        .ok_or(err(Error::MathOverflow))?;
    put(tm, t::F, &b.f.to_le_bytes())?;
    put(tm, t::PLAT_CUM, &b.plat.to_le_bytes())?;
    put(tm, t::INDEX, &b.index.to_le_bytes())?;
    put(tm, t::ACC, &b.acc.to_le_bytes())?;
    put(tm, t::RESERVED, &b.reserved.to_le_bytes())?;
    put(src, s::OWED, &add(owed, step.buyers)?.to_le_bytes())?;
    put(src, s::PLATFORM_DUE, &add(platform_due, step.plat)?.to_le_bytes())?;
    put(src, s::CREATOR_DUE, &add(creator_due, step.seller)?.to_le_bytes())
}

/// The pump route of this mint is fixed to the receiver: sharing config of the mint, active,
/// version 2, admin revoked, exactly one shareholder = receiver at 100%. Pump's global admin
/// (`admin_cto_sharing_config`) can re-open it, so this is re-checked on every list and buy.
fn route_fixed(sc: &AccountView, mint: &Address, recv: &Address) -> ProgramResult {
    need(
        sc.owned_by(&PUMP_FEES) && sc.address() == &pda_of(&[b"sharing-config", mint.as_ref()], &PUMP_FEES),
        Error::BadPumpAccount,
    )?;
    let data = sc.try_borrow()?;
    // SharingConfig: disc, bump@8, version@9, status@10, mint@11, admin@43, revoked@75, vec len@76, [addr, bps]@80.
    need(rd::<8>(&data, 0)? == SC_DISC && byte(&data, 9)? == 2 && byte(&data, 10)? == 1, Error::BadShares)?;
    need(key(&data, 11)? == *mint, Error::BadPumpAccount)?;
    need(
        byte(&data, 75)? == 1 && u32_at(&data, 76)? == 1 && key(&data, 80)? == *recv && u16_at(&data, 112)? == 10_000,
        Error::BadShares,
    )
}

/// Bonding curve of `mint` (pump IDL): disc, 5 x u64, complete@48, creator@49, mayhem@81,
/// cashback@82, quote_mint@83, creator_fee_bps@115, can_edit@123, holder_reward@124, creator_fee@125.
/// Returns (complete, creator, unswept creator fee).
fn curve(bc: &AccountView, mint: &Address) -> Result<(bool, Address, u64), ProgramError> {
    need(
        bc.owned_by(&PUMP) && bc.address() == &pda_of(&[b"bonding-curve", mint.as_ref()], &PUMP),
        Error::BadPumpAccount,
    )?;
    let data = bc.try_borrow()?;
    need(data.len() >= 125 && rd::<8>(&data, 0)? == BC_DISC, Error::BadPumpAccount)?;
    let complete = byte(&data, 48)?;
    need(complete <= 1, Error::BadPumpAccount)?;
    // SOL pair only; mayhem, cashback and holder-reward coins are excluded.
    need(
        key(&data, 83)? == ZERO && byte(&data, 81)? == 0 && byte(&data, 82)? == 0 && byte(&data, 124)? == 0,
        Error::BadPumpAccount,
    )?;
    Ok((complete == 1, key(&data, 49)?, u64_or_zero(&data, 125)))
}

/// Canonical PumpSwap pool the pump migration created for `mint` (pump_amm IDL Pool):
/// disc, bump@8, index@9, creator@11, base_mint@43, quote_mint@75, coin_creator@211, mayhem@243,
/// cashback@244, holder_reward@270, creator_fees@279. Returns (coin_creator, unswept creator fees).
fn pool(pool: &AccountView, mint: &Address) -> Result<(Address, u64), ProgramError> {
    let authority = pda_of(&[b"pool-authority", mint.as_ref()], &PUMP);
    need(
        pool.owned_by(&AMM)
            && pool.address()
                == &pda_of(&[b"pool", &0u16.to_le_bytes(), authority.as_ref(), mint.as_ref(), WSOL.as_ref()], &AMM),
        Error::BadPumpAccount,
    )?;
    let data = pool.try_borrow()?;
    need(data.len() >= 271 && rd::<8>(&data, 0)? == POOL_DISC, Error::BadPumpAccount)?;
    need(
        u16_at(&data, 9)? == 0
            && key(&data, 11)? == authority
            && key(&data, 43)? == *mint
            && key(&data, 75)? == WSOL
            && byte(&data, 243)? == 0
            && byte(&data, 244)? == 0
            && byte(&data, 270)? == 0,
        Error::BadPumpAccount,
    )?;
    Ok((key(&data, 211)?, u64_or_zero(&data, 279)))
}

/// A buy must follow sweep + distribute in the same tx: nothing (beyond dust) may wait upstream,
/// otherwise income earned before the purchase would be booked after it.
/// rest = [] for curve coins, [pool, amm creator-vault WSOL ATA] after migration.
fn swept(sc: &AccountView, bc: &AccountView, vault: &AccountView, rest: &[AccountView], mint: &Address) -> ProgramResult {
    let (complete, _, unswept) = curve(bc, mint)?;
    need(unswept <= MIN_COLLECT, Error::FeesNotSwept)?;
    need(vault.address() == &pda_of(&[b"creator-vault", sc.address().as_ref()], &PUMP), Error::BadPumpAccount)?;
    need(vault.lamports() <= add(rent(0)?, MIN_COLLECT)?, Error::FeesNotSwept)?;
    if !complete {
        return Ok(());
    }
    let [pl, ata, ..] = rest else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let (coin_creator, pool_fees) = pool(pl, mint)?;
    need(coin_creator == *sc.address(), Error::BadShares)?;
    need(pool_fees <= MIN_COLLECT, Error::FeesNotSwept)?;
    let authority = pda_of(&[b"creator_vault", sc.address().as_ref()], &AMM);
    need(
        ata.address() == &pda_of(&[authority.as_ref(), TOKEN.as_ref(), WSOL.as_ref()], &ATA),
        Error::BadPumpAccount,
    )?;
    if ata.owned_by(&TOKEN) {
        // SPL token account: mint@0, owner@32, amount@64
        let data = ata.try_borrow()?;
        need(key(&data, 0)? == WSOL && u64_at(&data, 64)? <= MIN_COLLECT, Error::FeesNotSwept)?;
    }
    Ok(())
}

pub fn process_instruction(pid: &Address, a: &mut [AccountView], d: &[u8]) -> ProgramResult {
    need(pid == &ID, Error::Unauthorized)?;
    let (&tag, args) = d.split_first().ok_or(ProgramError::InvalidInstructionData)?;
    match tag {
        0 => init_config(a, args),
        1 => set_config(a, args),
        2 => register_source(a, args),
        3 => list_term(a, args),
        4 => buy(a, args),
        5 => {
            let wound_down = a.first().is_some_and(|s| s.owned_by(&ID) && s.data_len() == x::LEN);
            if wound_down {
                forward(a)
            } else {
                collect(a)
            }
        }
        6 => settle(a, args),
        7 => claim(a, false),
        8 => claim(a, true),
        9 => close_term(a),
        10 => set_payout(a, args),
        11 => end_term_early(a),
        12 => close_source(a),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

/// admin / platform may not be this program, its config, the system program, zero; the platform
/// may not be uncreditable (executable, sysvar, reserved, our state). The admin signs to accept.
fn admin_platform(admin: &AccountView, platform: &AccountView, cfg: &Address) -> ProgramResult {
    signer(admin)?;
    for k in [admin.address(), platform.address()] {
        need(*k != ZERO && k != &ID && k != cfg && *k != SYSTEM, Error::BadParams)?;
    }
    need(!uncreditable(platform), Error::BadParams)
}

/// [authority(S,W), config(W), program, programdata, system, platform, admin(S)]; args: admin, platform.
/// Only the program's upgrade authority may create the config (no init front-running).
fn init_config(a: &mut [AccountView], d: &[u8]) -> ProgramResult {
    let [authority, cfg, program, pd, sys, platform, admin_acc, ..] = a else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    signer(authority)?;
    system(sys)?;
    need(program.address() == &ID && program.owned_by(&LOADER) && program.executable(), Error::Unauthorized)?;
    let pd_key = pda_of(&[ID.as_ref()], &LOADER);
    need(pd.address() == &pd_key && pd.owned_by(&LOADER), Error::Unauthorized)?;
    {
        let data = program.try_borrow()?;
        need(rd::<4>(&data, 0)? == 2u32.to_le_bytes() && key(&data, 4)? == pd_key, Error::Unauthorized)?;
        let data = pd.try_borrow()?;
        need(
            rd::<4>(&data, 0)? == 3u32.to_le_bytes() && byte(&data, 12)? == 1 && key(&data, 13)? == *authority.address(),
            Error::Unauthorized,
        )?;
    }
    let admin = key(d, 0)?;
    need(admin_acc.address() == &admin && platform.address() == &key(d, 32)?, Error::BadParams)?;
    let bump = [find(&[b"config"], cfg)?];
    admin_platform(admin_acc, platform, cfg.address())?;
    init_account(authority, cfg, c::LEN, &[Seed::from(b"config"), Seed::from(&bump)])?;
    let mut cd = [0u8; c::LEN];
    cd[0] = CONFIG;
    cd[c::BUMP] = bump[0];
    put(&mut cd, c::ADMIN, admin.as_ref())?;
    put(&mut cd, c::PLATFORM, platform.address().as_ref())?;
    put(&mut cd, c::PLAT_BPS, &PLAT_BPS.to_le_bytes())?;
    cd[c::PAUSED] = 1; // opened by the admin when ready
    save(cfg, &cd)
}

/// [admin(S), config(W), platform, new_admin(S) (may be the same key)];
/// args: admin, platform, fill_fee_bps u16 (0..=200), paused u8.
fn set_config(a: &mut [AccountView], d: &[u8]) -> ProgramResult {
    let [admin, cfg, platform, new_admin_acc, ..] = a else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let mut cd = config(cfg)?;
    signer(admin)?;
    need(admin.address() == &key(&cd, c::ADMIN)?, Error::Unauthorized)?;
    let new_admin = key(d, 0)?;
    need(new_admin_acc.address() == &new_admin && platform.address() == &key(d, 32)?, Error::BadParams)?;
    let fill = u16::from_le_bytes(rd(d, 64)?);
    let paused = byte(d, 66)?;
    need(fill <= MAX_FILL_BPS && paused <= 1, Error::BadParams)?;
    admin_platform(new_admin_acc, platform, cfg.address())?;
    put(&mut cd, c::ADMIN, new_admin.as_ref())?;
    put(&mut cd, c::PLATFORM, platform.address().as_ref())?;
    put(&mut cd, c::FILL_BPS, &fill.to_le_bytes())?;
    cd[c::PAUSED] = paused;
    save(cfg, &cd)
}

/// [creator(S,W), config, source(W), bonding_curve, system, (pool if migrated)]; args: mint.
/// Signer = bonding_curve.creator before migration, Pool.coin_creator after (canonical pump pool).
/// Must run BEFORE the pump onboarding tx: afterwards the creator field is the sharing config.
fn register_source(a: &mut [AccountView], d: &[u8]) -> ProgramResult {
    let [creator, cfg, src, bc, sys, rest @ ..] = a else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    live(&config(cfg)?)?;
    system(sys)?;
    let mint = key(d, 0)?;
    let (complete, curve_creator, _) = curve(bc, &mint)?;
    let owner = if complete {
        let [pl, ..] = rest else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };
        pool(pl, &mint)?.0
    } else {
        curve_creator
    };
    need(owner == *creator.address(), Error::Unauthorized)?;
    let bump = [find(&[b"recv", mint.as_ref()], src)?];
    init_account(creator, src, s::LEN, &[Seed::from(b"recv"), Seed::from(mint.as_ref()), Seed::from(&bump)])?;
    let mut sd = [0u8; s::LEN];
    sd[0] = SOURCE;
    sd[s::BUMP] = bump[0];
    put(&mut sd, s::MINT, mint.as_ref())?;
    put(&mut sd, s::CREATOR, creator.address().as_ref())?;
    put(&mut sd, s::PAYOUT, creator.address().as_ref())?;
    save(src, &sd)
}

/// [creator(S,W), config, source(W), term(W), sharing_config, system];
/// args: share_bps u16, units u32, days u8 (7|30), price u64.
fn list_term(a: &mut [AccountView], d: &[u8]) -> ProgramResult {
    let [creator, cfg, src, tm, sc, sys, ..] = a else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let cd = config(cfg)?;
    live(&cd)?;
    system(sys)?;
    writable(src)?;
    let mut sd = source(src)?;
    signer(creator)?;
    need(creator.address() == &key(&sd, s::CREATOR)?, Error::Unauthorized)?;
    let share = u16::from_le_bytes(rd(d, 0)?);
    let units = u32::from_le_bytes(rd(d, 2)?);
    let days = byte(d, 6)?;
    let price = u64::from_le_bytes(rd(d, 7)?);
    need(
        (500..=5000).contains(&share) && (10..=10_000).contains(&units) && (days == 7 || days == 30) && price >= MIN_PRICE,
        Error::BadParams,
    )?;
    need(price.checked_mul(units as u64).is_some(), Error::BadParams)?;
    let now = now()?;
    need(now >= i64_at(&sd, s::END)?, Error::TermActive)?;
    // Income booked before the term belongs to the creator.
    book(&mut sd, src.lamports(), None, now, true)?;
    let mint = key(&sd, s::MINT)?;
    route_fixed(sc, &mint, src.address())?;
    let seq = add(u64_at(&sd, s::SEQ)?, 1)?.to_le_bytes();
    let bump = [find(&[b"term", mint.as_ref(), &seq], tm)?];
    init_account(
        creator,
        tm,
        t::LEN,
        &[Seed::from(b"term"), Seed::from(mint.as_ref()), Seed::from(&seq), Seed::from(&bump)],
    )?;
    let end = now.checked_add(days as i64 * DAY).ok_or(err(Error::MathOverflow))?;
    let mut td = [0u8; t::LEN];
    td[0] = TERM;
    td[t::BUMP] = bump[0];
    put(&mut td, t::MINT, mint.as_ref())?;
    put(&mut td, t::SEQ, &seq)?;
    put(&mut td, t::START, &now.to_le_bytes())?;
    put(&mut td, t::END, &end.to_le_bytes())?;
    put(&mut td, t::SHARE, &share.to_le_bytes())?;
    put(&mut td, t::PLAT, &rd::<2>(&cd, c::PLAT_BPS)?)?;
    put(&mut td, t::FILL, &rd::<2>(&cd, c::FILL_BPS)?)?;
    put(&mut td, t::UNITS, &units.to_le_bytes())?;
    put(&mut td, t::PRICE, &price.to_le_bytes())?;
    put(&mut sd, s::SEQ, &seq)?;
    put(&mut sd, s::END, &end.to_le_bytes())?;
    put(&mut sd, s::TERM, tm.address().as_ref())?;
    let open = u32_at(&sd, s::OPEN_TERMS)?.checked_add(1).ok_or(err(Error::MathOverflow))?;
    put(&mut sd, s::OPEN_TERMS, &open.to_le_bytes())?;
    save(tm, &td)?;
    save(src, &sd)
}

/// [buyer(S,W), config, source(W), term(W), position(W), payout(W) = source payout, platform(W), system,
///  sharing_config, bonding_curve, pump creator vault, (pool, amm creator-vault WSOL ATA if migrated)];
/// args: units u32. The server puts pump sweep(s) (+ AMM transfer) + distribute before this in the tx.
fn buy(a: &mut [AccountView], d: &[u8]) -> ProgramResult {
    let [buyer, cfg, src, tm, pos, payout, platform, sys, sc, bc, vault, rest @ ..] = a else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let cd = config(cfg)?;
    live(&cd)?;
    system(sys)?;
    signer(buyer)?;
    writable(src)?;
    let mut sd = source(src)?;
    let mut td = term(tm, &sd)?;
    let mint = key(&sd, s::MINT)?;
    need(tm.address() == &key(&sd, s::TERM)?, Error::BadAccount)?;
    need(payout.address() == &key(&sd, s::PAYOUT)?, Error::Unauthorized)?;
    need(platform.address() == &key(&cd, c::PLATFORM)?, Error::Unauthorized)?;
    need(buyer.address() != &key(&sd, s::CREATOR)? && buyer.address() != payout.address(), Error::BadParams)?;
    route_fixed(sc, &mint, src.address())?;
    swept(sc, bc, vault, rest, &mint)?;
    let now = now()?;
    need(now < i64_at(&td, t::END)?, Error::TermEnded)?;
    // Book pending income first so a purchase never shares fees that arrived before it.
    book(&mut sd, src.lamports(), Some(&mut td), now, true)?;
    let units = u32::from_le_bytes(rd(d, 0)?);
    let sold = u32_at(&td, t::SOLD)?;
    let left = u32_at(&td, t::UNITS)?.checked_sub(sold).ok_or(err(Error::MathOverflow))?;
    need(units > 0, Error::BadParams)?;
    need(units <= left, Error::SoldOut)?;
    let cost = u64_at(&td, t::PRICE)?.checked_mul(units as u64).ok_or(err(Error::MathOverflow))?;
    let fill = mul_div(cost, u16_at(&td, t::FILL)? as u64, BPS).ok_or(err(Error::MathOverflow))?;
    Transfer { from: buyer, to: payout, lamports: cost }.invoke()?;
    if fill > 0 {
        Transfer { from: buyer, to: platform, lamports: fill }.invoke()?;
    }
    let index = u128_at(&td, t::INDEX)?;
    let mut pd = if pos.owned_by(&SYSTEM) && pos.data_len() == 0 {
        let bump = [find(&[b"pos", tm.address().as_ref(), buyer.address().as_ref()], pos)?];
        init_account(
            buyer,
            pos,
            p::LEN,
            &[Seed::from(b"pos"), Seed::from(tm.address().as_ref()), Seed::from(buyer.address().as_ref()), Seed::from(&bump)],
        )?;
        let mut pd = [0u8; p::LEN];
        pd[0] = POSITION;
        pd[p::BUMP] = bump[0];
        put(&mut pd, p::TERM, tm.address().as_ref())?;
        put(&mut pd, p::OWNER, buyer.address().as_ref())?;
        put(&mut pd, p::PAYOUT, buyer.address().as_ref())?;
        let open = u32_at(&td, t::OPEN)?.checked_add(1).ok_or(err(Error::MathOverflow))?;
        put(&mut td, t::OPEN, &open.to_le_bytes())?;
        pd
    } else {
        let mut pd = position(pos)?;
        need(key(&pd, p::TERM)? == *tm.address() && key(&pd, p::OWNER)? == *buyer.address(), Error::Unauthorized)?;
        // Fix what the old units earned before the entry index moves.
        let got = earned(u32_at(&pd, p::UNITS)?, index, u128_at(&pd, p::ENTRY)?).ok_or(err(Error::MathOverflow))?;
        inc(&mut pd, p::ACCRUED, got)?;
        pd
    };
    let held = u32_at(&pd, p::UNITS)?.checked_add(units).ok_or(err(Error::MathOverflow))?;
    put(&mut pd, p::UNITS, &held.to_le_bytes())?;
    put(&mut pd, p::ENTRY, &index.to_le_bytes())?;
    put(&mut td, t::SOLD, &(sold + units).to_le_bytes())?;
    save(pos, &pd)?;
    save(tm, &td)?;
    save(src, &sd)
}

/// Permissionless. [source(W), term(W)]: term = the source's latest term (ignored outside it).
/// The keeper puts pump sweep(s) (+ AMM transfer) + distribute_creator_fees_v2 before this in one tx.
fn collect(a: &mut [AccountView]) -> ProgramResult {
    let [src, tm, ..] = a else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    writable(src)?;
    let mut sd = source(src)?;
    let now = now()?;
    if now < i64_at(&sd, s::END)? {
        need(tm.address() == &key(&sd, s::TERM)?, Error::BadAccount)?;
        let mut td = term(tm, &sd)?;
        book(&mut sd, src.lamports(), Some(&mut td), now, false)?;
        save(tm, &td)?;
    } else {
        book(&mut sd, src.lamports(), None, now, false)?;
    }
    save(src, &sd)
}

/// Permissionless. [source(W), destination(W), config, platform(W)]; args: 0 = creator share
/// (destination = source payout; an uncreditable payout reroutes to platform), 1 = platform share.
/// Pull-based so a broken destination never blocks collects or claims.
fn settle(a: &mut [AccountView], d: &[u8]) -> ProgramResult {
    let [src, dest, cfg, platform, ..] = a else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let cd = config(cfg)?;
    let mut sd = source(src)?;
    let who = byte(d, 0)?;
    let field = match who {
        0 => s::CREATOR_DUE,
        1 => s::PLATFORM_DUE,
        _ => return Err(ProgramError::InvalidInstructionData),
    };
    let amount = u64_at(&sd, field)?;
    need(amount > 0, Error::NothingToClaim)?;
    put(&mut sd, field, &0u64.to_le_bytes())?;
    save(src, &sd)?;
    let to = if who == 0 { key(&sd, s::PAYOUT)? } else { key(&cd, c::PLATFORM)? };
    pay_out(src, dest, &to, platform, &cd, amount, 0)
}

/// claim: [owner(S,W), source(W), term(W), position(W), payout(W), config, platform(W)].
/// refund_position (admin, after the term ends): [admin(S), config, source(W), term(W), position(W),
/// owner(W), payout(W), platform(W)].
/// Pays everything accrued to the position's payout; at/after the term's end the position is closed
/// and its rent returned to the owner (the original payer).
fn claim(a: &mut [AccountView], admin: bool) -> ProgramResult {
    let (cfg, src, tm, pos, owner, payout, platform) = if admin {
        let [adm, cfg, src, tm, pos, owner, payout, platform, ..] = a else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };
        let cd = config(cfg)?;
        signer(adm)?;
        need(adm.address() == &key(&cd, c::ADMIN)?, Error::Unauthorized)?;
        (cfg, src, tm, pos, owner, payout, platform)
    } else {
        let [owner, src, tm, pos, payout, cfg, platform, ..] = a else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };
        signer(owner)?;
        (cfg, src, tm, pos, owner, payout, platform)
    };
    let cd = config(cfg)?;
    let mut sd = source(src)?;
    let mut td = term(tm, &sd)?;
    let mut pd = position(pos)?;
    need(key(&pd, p::TERM)? == *tm.address() && key(&pd, p::OWNER)? == *owner.address(), Error::Unauthorized)?;
    let ended = now()? >= i64_at(&td, t::END)?;
    need(ended || !admin, Error::TermNotEnded)?;
    let index = u128_at(&td, t::INDEX)?;
    let due = add(
        u64_at(&pd, p::ACCRUED)?,
        earned(u32_at(&pd, p::UNITS)?, index, u128_at(&pd, p::ENTRY)?).ok_or(err(Error::MathOverflow))?,
    )?;
    need(due > 0 || ended, Error::NothingToClaim)?;
    put(&mut pd, p::ACCRUED, &0u64.to_le_bytes())?;
    put(&mut pd, p::ENTRY, &index.to_le_bytes())?;
    inc(&mut pd, p::CLAIMED, due)?;
    inc(&mut td, t::CLAIMED, due)?;
    dec(&mut sd, s::OWED, due)?;
    if ended {
        let open = u32_at(&td, t::OPEN)?.checked_sub(1).ok_or(err(Error::MathOverflow))?;
        put(&mut td, t::OPEN, &open.to_le_bytes())?;
    }
    save(src, &sd)?;
    save(tm, &td)?;
    let payout_key = key(&pd, p::PAYOUT)?;
    // closing: the position rent goes to the owner in this same instruction
    let extra = if ended && payout_key == *owner.address() { pos.lamports() } else { 0 };
    pay_out(src, payout, &payout_key, platform, &cd, due, extra)?;
    need(src.lamports() >= rent(s::LEN)?, Error::Insolvent)?;
    if ended {
        close(pos, owner)
    } else {
        save(pos, &pd)
    }
}

/// Permissionless after the end once every position is closed. [source(W), term(W), creator(W)].
/// Rounding dust left in the term's reserve goes to the creator's due; term rent back to the creator
/// (the signer that paid it at listing).
fn close_term(a: &mut [AccountView]) -> ProgramResult {
    let [src, tm, creator, ..] = a else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let mut sd = source(src)?;
    let td = term(tm, &sd)?;
    need(creator.address() == &key(&sd, s::CREATOR)?, Error::Unauthorized)?;
    need(now()? >= i64_at(&td, t::END)?, Error::TermNotEnded)?;
    need(u32_at(&td, t::OPEN)? == 0, Error::PositionsOpen)?;
    let dust = sub(u64_at(&td, t::RESERVED)?, u64_at(&td, t::CLAIMED)?)?;
    dec(&mut sd, s::OWED, dust)?;
    inc(&mut sd, s::CREATOR_DUE, dust)?;
    let open = u32_at(&sd, s::OPEN_TERMS)?.checked_sub(1).ok_or(err(Error::MathOverflow))?;
    put(&mut sd, s::OPEN_TERMS, &open.to_le_bytes())?;
    save(src, &sd)?;
    close(tm, creator)
}

/// [signer(S), target(W), payout]; args: payout. Target = Source / closed Source (signer = creator)
/// or a Position (signer = owner). Uncreditable addresses are refused up front, so a buy (direct
/// transfer to the creator payout) can never be stopped by the payout choice.
fn set_payout(a: &mut [AccountView], d: &[u8]) -> ProgramResult {
    let [who, target, new_payout, ..] = a else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    signer(who)?;
    let payout = key(d, 0)?;
    need(
        new_payout.address() == &payout && payout != ZERO && payout != ID && !uncreditable(new_payout),
        Error::BadParams,
    )?;
    let kind = if target.owned_by(&ID) { byte(&target.try_borrow()?, 0)? } else { 0 };
    if kind == CLOSED {
        let mut xd = closed(target)?;
        need(who.address() == &key(&xd, x::CREATOR)?, Error::Unauthorized)?;
        put(&mut xd, x::PAYOUT, payout.as_ref())?;
        save(target, &xd)
    } else if kind == SOURCE {
        let mut sd = source(target)?;
        need(who.address() == &key(&sd, s::CREATOR)?, Error::Unauthorized)?;
        put(&mut sd, s::PAYOUT, payout.as_ref())?;
        save(target, &sd)
    } else {
        let mut pd = position(target)?;
        need(who.address() == &key(&pd, p::OWNER)?, Error::Unauthorized)?;
        put(&mut pd, p::PAYOUT, payout.as_ref())?;
        save(target, &pd)
    }
}

/// Wind-down, admin while paused. [admin(S), config, source(W), term(W) = latest term].
/// Books pending income into the term, then ends it now: claims/refunds/close_term work at once.
fn end_term_early(a: &mut [AccountView]) -> ProgramResult {
    let [admin, cfg, src, tm, ..] = a else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let cd = config(cfg)?;
    signer(admin)?;
    need(admin.address() == &key(&cd, c::ADMIN)?, Error::Unauthorized)?;
    need(cd[c::PAUSED] == 1, Error::BadParams)?;
    let mut sd = source(src)?;
    need(tm.address() == &key(&sd, s::TERM)?, Error::BadAccount)?;
    let mut td = term(tm, &sd)?;
    let now = now()?;
    need(now < i64_at(&td, t::END)?, Error::TermEnded)?;
    book(&mut sd, src.lamports(), Some(&mut td), now, false)?;
    put(&mut td, t::END, &now.to_le_bytes())?;
    put(&mut sd, s::END, &now.to_le_bytes())?;
    save(tm, &td)?;
    save(src, &sd)
}

/// Wind-down, admin while paused, after every term is closed and every due settled.
/// [admin(S), config, source(W), payout(W), platform(W)].
/// Compacts the Source to the CLOSED layout and sends the unbooked remainder plus the rent difference
/// to the creator's payout in one transfer (platform if uncreditable). If an empty payout could not
/// hold that sum it stays on the compacted Source; collect forwards it later. The address stays pump's
/// permanent recipient; collect on it then forwards everything to the payout.
fn close_source(a: &mut [AccountView]) -> ProgramResult {
    let [admin, cfg, src, payout, platform, ..] = a else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let cd = config(cfg)?;
    signer(admin)?;
    need(admin.address() == &key(&cd, c::ADMIN)?, Error::Unauthorized)?;
    need(cd[c::PAUSED] == 1, Error::BadParams)?;
    let sd = source(src)?;
    need(now()? >= i64_at(&sd, s::END)?, Error::TermActive)?;
    need(u32_at(&sd, s::OPEN_TERMS)? == 0, Error::PositionsOpen)?;
    need(
        u64_at(&sd, s::OWED)? == 0 && u64_at(&sd, s::CREATOR_DUE)? == 0 && u64_at(&sd, s::PLATFORM_DUE)? == 0,
        Error::PositionsOpen,
    )?;
    let payout_key = key(&sd, s::PAYOUT)?;
    need(payout.address() == &payout_key, Error::Unauthorized)?;
    let all = sub(src.lamports(), rent(x::LEN)?)?;
    if uncreditable(payout) || payout.lamports() > 0 || all >= rent(0)? {
        pay_out(src, payout, &payout_key, platform, &cd, all, 0)?;
    }
    let mut xd = [0u8; x::LEN];
    xd[0] = CLOSED;
    xd[x::BUMP] = sd[s::BUMP];
    put(&mut xd, x::MINT, &sd[s::MINT..s::MINT + 32])?;
    put(&mut xd, x::CREATOR, &sd[s::CREATOR..s::CREATOR + 32])?;
    put(&mut xd, x::PAYOUT, &sd[s::PAYOUT..s::PAYOUT + 32])?;
    src.try_borrow_mut()?.fill(0);
    src.resize(x::LEN)?;
    save(src, &xd)
}

/// collect on a compacted Source: permissionless. [source(W), payout(W), config, platform(W)].
/// Everything above rent goes to the creator's payout (platform if it can never be credited).
fn forward(a: &mut [AccountView]) -> ProgramResult {
    let [src, payout, cfg, platform, ..] = a else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let cd = config(cfg)?;
    let xd = closed(src)?;
    let amount = sub(src.lamports(), rent(x::LEN)?)?;
    need(amount > 0, Error::NothingToClaim)?;
    pay_out(src, payout, &key(&xd, x::PAYOUT)?, platform, &cd, amount, 0)
}
