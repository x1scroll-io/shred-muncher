use anchor_lang::prelude::*;
use anchor_lang::system_program;

declare_id!("4jekyzVvjUDzUydX7b5vBBi4tX5BJZQDjZkC8hMcvbNn"); // replace after deploy

// ── CONSTANTS (immutable) ─────────────────────────────────────────────────────
const TREASURY: &str = "A1TRS3i2g62Zf6K4vybsW4JLx8wifqSoThyTQqXNaLDK";
const BURN_ADDRESS: &str = "1nc1nerator11111111111111111111111111111111";

const TREASURY_BPS: u64 = 5000;
const BURN_BPS: u64 = 5000;
const BASIS_POINTS: u64 = 10000;

// Muncher node bond: 500 XNT minimum (slashable for bad cleanup)
const MIN_MUNCHER_BOND: u64 = 500_000_000_000;

// Slash: 10% of bond for bad resolution
const SLASH_BPS: u64 = 1000;

// Cleanup fee paid to muncher: 0.001 XNT per shred → 50/50 treasury/burn
const CLEANUP_FEE: u64 = 1_000_000;

// Subscription: 5 XNT / 90 epochs for cleanup service access
const SUBSCRIPTION_FEE: u64 = 5_000_000_000;
const SUBSCRIPTION_EPOCHS: u64 = 90;

// Max muncher nodes
const MAX_MUNCHERS: usize = 50;

// Max shred log entries
const MAX_SHRED_LOG: usize = 1000;

/// Shred types — what gets munched
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq)]
pub enum ShredType {
    OrphanedTx,         // Valid tx but parent failed
    StuckBundle,        // Multi-TX partial failure
    FailedSimulation,   // Pre-flight error propagating
    ForkDebris,         // Post-fork invalid TXs
    GossipNoise,        // Malformed/attack TXs
    StaleMempool,       // Too-old pending TXs
}

/// Severity classification
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq)]
pub enum Severity {
    Critical,   // Breaks consensus — immediate action
    High,       // Stuck bundles — queue for cleanup
    Medium,     // Orphaned TXs — monitor
    Low,        // Edge cases — log only
}

/// Resolution action taken
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq)]
pub enum Resolution {
    Rebroadcast,    // Re-sent through healthy validator
    PriorityBump,   // Bumped with priority fee
    AtomicCancel,   // Cancelled entire bundle
    Pruned,         // Removed from local cache
    Dropped,        // Discarded (gossip noise)
    Logged,         // Logged only (low severity)
}

/// Geographic region for node placement
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq)]
pub enum Region {
    NorthAmerica,
    Europe,
    AsiaPacific,
    Edge,
}

#[program]
pub mod shred_muncher {
    use super::*;

    pub fn initialize(ctx: Context<Initialize>) -> Result<()> {
        let state = &mut ctx.accounts.state;
        state.authority = ctx.accounts.authority.key();
        state.muncher_count = 0;
        state.shred_count = 0;
        state.total_shreds_munched = 0;
        state.total_fees_collected = 0;
        state.total_burned = 0;
        state.bump = ctx.bumps.state;
        Ok(())
    }

    /// Register as a Muncher Node
    /// Bond 500 XNT — slashed for bad cleanup actions
    /// Node agrees to maintain strategic RPC infrastructure
    pub fn register_muncher(
        ctx: Context<RegisterMuncher>,
        bond_amount: u64,
        region: Region,
        rpc_endpoint: [u8; 64],    // node's RPC endpoint (encoded)
    ) -> Result<()> {
        require!(bond_amount >= MIN_MUNCHER_BOND, MuncherError::BondTooSmall);

        let state = &mut ctx.accounts.state;
        require!((state.muncher_count as usize) < MAX_MUNCHERS, MuncherError::TooManyMunchers);

        let operator = ctx.accounts.operator.key();
        for i in 0..state.muncher_count as usize {
            require!(state.munchers[i].operator != operator, MuncherError::AlreadyRegistered);
        }

        // Lock bond
        system_program::transfer(CpiContext::new(ctx.accounts.system_program.to_account_info(),
            system_program::Transfer { from: ctx.accounts.operator.to_account_info(), to: ctx.accounts.bond_vault.to_account_info() }),
            bond_amount)?;

        let idx = state.muncher_count as usize;
        state.munchers[idx] = MuncherNode {
            operator,
            bond_amount,
            region,
            rpc_endpoint,
            shreds_munched: 0,
            bad_cleanups: 0,
            active: true,
            registered_epoch: Clock::get()?.epoch,
        };
        state.muncher_count += 1;

        emit!(MuncherRegistered { operator, bond: bond_amount, region, epoch: Clock::get()?.epoch });
        Ok(())
    }

    /// Log a shred cleanup action on-chain
    /// Muncher node reports what it detected and how it resolved it
    /// Fees collected per shred → 50/50 treasury/burn
    pub fn log_cleanup(
        ctx: Context<LogCleanup>,
        shred_type: ShredType,
        severity: Severity,
        original_sig: [u8; 64],     // signature of the problematic tx
        resolution: Resolution,
        slot_detected: u64,
        affected_validators: u8,    // how many validators this affected
    ) -> Result<()> {
        let state = &mut ctx.accounts.state;
        let muncher = ctx.accounts.muncher.key();

        // Verify muncher is registered
        let mut muncher_idx = None;
        for i in 0..state.muncher_count as usize {
            if state.munchers[i].operator == muncher && state.munchers[i].active {
                muncher_idx = Some(i);
                break;
            }
        }
        require!(muncher_idx.is_some(), MuncherError::NotAMuncher);

        // Collect cleanup fee → 50/50 treasury/burn
        let treasury_fee = CLEANUP_FEE * TREASURY_BPS / BASIS_POINTS;
        let burn_fee = CLEANUP_FEE - treasury_fee;

        system_program::transfer(CpiContext::new(ctx.accounts.system_program.to_account_info(),
            system_program::Transfer { from: ctx.accounts.fee_payer.to_account_info(), to: ctx.accounts.treasury.to_account_info() }),
            treasury_fee)?;
        system_program::transfer(CpiContext::new(ctx.accounts.system_program.to_account_info(),
            system_program::Transfer { from: ctx.accounts.fee_payer.to_account_info(), to: ctx.accounts.burn_address.to_account_info() }),
            burn_fee)?;

        // Log shred cleanup
        if (state.shred_count as usize) < MAX_SHRED_LOG {
            let sidx = state.shred_count as usize;
            state.shred_log[sidx] = ShredLog {
                shred_type,
                severity,
                original_sig,
                resolution,
                muncher_node: muncher,
                slot_detected,
                affected_validators,
                logged_slot: Clock::get()?.slot,
                disputed: false,
            };
            state.shred_count += 1;
        }

        state.munchers[muncher_idx.unwrap()].shreds_munched += 1;
        state.total_shreds_munched += 1;
        state.total_fees_collected = state.total_fees_collected.checked_add(CLEANUP_FEE).ok_or(MuncherError::MathOverflow)?;
        state.total_burned = state.total_burned.checked_add(burn_fee).ok_or(MuncherError::MathOverflow)?;

        emit!(ShredMunched {
            shred_type,
            severity,
            resolution,
            muncher,
            slot_detected,
            affected_validators,
            fee: CLEANUP_FEE,
            burned: burn_fee,
            slot: Clock::get()?.slot,
        });

        Ok(())
    }

    /// Subscribe to Muncher cleanup service
    /// dApps and wallets pay for priority cleanup of their transactions
    /// 5 XNT / 90 epochs → 50/50 treasury/burn
    pub fn subscribe(ctx: Context<Subscribe>) -> Result<()> {
        let treasury_fee = SUBSCRIPTION_FEE * TREASURY_BPS / BASIS_POINTS;
        let burn_fee = SUBSCRIPTION_FEE - treasury_fee;

        system_program::transfer(CpiContext::new(ctx.accounts.system_program.to_account_info(),
            system_program::Transfer { from: ctx.accounts.subscriber.to_account_info(), to: ctx.accounts.treasury.to_account_info() }),
            treasury_fee)?;
        system_program::transfer(CpiContext::new(ctx.accounts.system_program.to_account_info(),
            system_program::Transfer { from: ctx.accounts.subscriber.to_account_info(), to: ctx.accounts.burn_address.to_account_info() }),
            burn_fee)?;

        let state = &mut ctx.accounts.state;
        state.total_fees_collected = state.total_fees_collected.checked_add(SUBSCRIPTION_FEE).ok_or(MuncherError::MathOverflow)?;
        state.total_burned = state.total_burned.checked_add(burn_fee).ok_or(MuncherError::MathOverflow)?;

        let current_epoch = Clock::get()?.epoch;
        emit!(Subscribed {
            subscriber: ctx.accounts.subscriber.key(),
            expires_epoch: current_epoch + SUBSCRIPTION_EPOCHS,
            fee: SUBSCRIPTION_FEE,
            burned: burn_fee,
            epoch: current_epoch,
        });

        Ok(())
    }

    /// Dispute a cleanup action — another muncher challenges bad resolution
    /// Requires stake — slashes bad muncher if upheld
    pub fn dispute_cleanup(
        ctx: Context<DisputeCleanup>,
        shred_log_index: u32,
        reason: [u8; 128],
    ) -> Result<()> {
        let state = &mut ctx.accounts.state;
        let challenger = ctx.accounts.challenger.key();
        let lidx = shred_log_index as usize;

        require!(lidx < state.shred_count as usize, MuncherError::LogNotFound);
        require!(!state.shred_log[lidx].disputed, MuncherError::AlreadyDisputed);

        // Verify challenger is a muncher
        let mut is_muncher = false;
        for i in 0..state.muncher_count as usize {
            if state.munchers[i].operator == challenger && state.munchers[i].active {
                is_muncher = true;
                break;
            }
        }
        require!(is_muncher, MuncherError::NotAMuncher);

        state.shred_log[lidx].disputed = true;

        emit!(CleanupDisputed {
            shred_log_index,
            challenger,
            disputed_muncher: state.shred_log[lidx].muncher_node,
            slot: Clock::get()?.slot,
        });

        Ok(())
    }

    /// Slash a muncher for bad cleanup (authority resolves disputes)
    pub fn slash_muncher(
        ctx: Context<SlashMuncher>,
        operator_identity: Pubkey,
    ) -> Result<()> {
        let state = &mut ctx.accounts.state;
        require!(ctx.accounts.authority.key() == state.authority, MuncherError::Unauthorized);

        for i in 0..state.muncher_count as usize {
            if state.munchers[i].operator == operator_identity {
                let slash = state.munchers[i].bond_amount * SLASH_BPS / BASIS_POINTS;
                let treasury_cut = slash * TREASURY_BPS / BASIS_POINTS;
                let burn_cut = slash - treasury_cut;

                system_program::transfer(CpiContext::new_with_signer(ctx.accounts.system_program.to_account_info(),
                    system_program::Transfer { from: ctx.accounts.bond_vault.to_account_info(), to: ctx.accounts.treasury.to_account_info() },
                    &[&[b"muncher-vault", &[ctx.bumps.bond_vault]]]), treasury_cut)?;
                system_program::transfer(CpiContext::new_with_signer(ctx.accounts.system_program.to_account_info(),
                    system_program::Transfer { from: ctx.accounts.bond_vault.to_account_info(), to: ctx.accounts.burn_address.to_account_info() },
                    &[&[b"muncher-vault", &[ctx.bumps.bond_vault]]]), burn_cut)?;

                state.munchers[i].bond_amount = state.munchers[i].bond_amount.saturating_sub(slash);
                state.munchers[i].bad_cleanups += 1;
                state.total_burned = state.total_burned.checked_add(burn_cut).ok_or(MuncherError::MathOverflow)?;

                emit!(MuncherSlashed { operator: operator_identity, slash_amount: slash, burned: burn_cut, epoch: Clock::get()?.epoch });
                return Ok(());
            }
        }
        Err(MuncherError::MuncherNotFound.into())
    }
}

// ── ACCOUNTS ──────────────────────────────────────────────────────────────────

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(init, payer = authority, space = 8 + MuncherState::LEN, seeds = [b"shred-muncher"], bump)]
    pub state: Account<'info, MuncherState>,
    #[account(mut)]
    pub authority: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct RegisterMuncher<'info> {
    #[account(mut, seeds = [b"shred-muncher"], bump = state.bump)]
    pub state: Account<'info, MuncherState>,
    #[account(mut)]
    pub operator: Signer<'info>,
    /// CHECK: bond vault
    #[account(mut, seeds = [b"muncher-vault"], bump)]
    pub bond_vault: AccountInfo<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct LogCleanup<'info> {
    #[account(mut, seeds = [b"shred-muncher"], bump = state.bump)]
    pub state: Account<'info, MuncherState>,
    pub muncher: Signer<'info>,
    #[account(mut)]
    pub fee_payer: Signer<'info>,
    /// CHECK: treasury
    #[account(mut, constraint = treasury.key().to_string() == TREASURY @ MuncherError::InvalidTreasury)]
    pub treasury: AccountInfo<'info>,
    /// CHECK: burn
    #[account(mut, constraint = burn_address.key().to_string() == BURN_ADDRESS @ MuncherError::InvalidBurnAddress)]
    pub burn_address: AccountInfo<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Subscribe<'info> {
    #[account(mut, seeds = [b"shred-muncher"], bump = state.bump)]
    pub state: Account<'info, MuncherState>,
    #[account(mut)]
    pub subscriber: Signer<'info>,
    /// CHECK: treasury
    #[account(mut, constraint = treasury.key().to_string() == TREASURY @ MuncherError::InvalidTreasury)]
    pub treasury: AccountInfo<'info>,
    /// CHECK: burn
    #[account(mut, constraint = burn_address.key().to_string() == BURN_ADDRESS @ MuncherError::InvalidBurnAddress)]
    pub burn_address: AccountInfo<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct DisputeCleanup<'info> {
    #[account(mut, seeds = [b"shred-muncher"], bump = state.bump)]
    pub state: Account<'info, MuncherState>,
    pub challenger: Signer<'info>,
}

#[derive(Accounts)]
pub struct SlashMuncher<'info> {
    #[account(mut, seeds = [b"shred-muncher"], bump = state.bump)]
    pub state: Account<'info, MuncherState>,
    pub authority: Signer<'info>,
    /// CHECK: bond vault
    #[account(mut, seeds = [b"muncher-vault"], bump)]
    pub bond_vault: AccountInfo<'info>,
    /// CHECK: treasury
    #[account(mut, constraint = treasury.key().to_string() == TREASURY @ MuncherError::InvalidTreasury)]
    pub treasury: AccountInfo<'info>,
    /// CHECK: burn
    #[account(mut, constraint = burn_address.key().to_string() == BURN_ADDRESS @ MuncherError::InvalidBurnAddress)]
    pub burn_address: AccountInfo<'info>,
    pub system_program: Program<'info, System>,
}

// ── STATE ─────────────────────────────────────────────────────────────────────

#[account]
pub struct MuncherState {
    pub authority: Pubkey,
    pub muncher_count: u32,
    pub shred_count: u32,
    pub total_shreds_munched: u64,
    pub total_fees_collected: u64,
    pub total_burned: u64,
    pub bump: u8,
    pub munchers: [MuncherNode; 50],
    pub shred_log: [ShredLog; 1000],
}

impl MuncherState {
    pub const LEN: usize = 32 + 4 + 4 + 8 + 8 + 8 + 1
        + (MuncherNode::LEN * 50)
        + (ShredLog::LEN * 1000);
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy)]
pub struct MuncherNode {
    pub operator: Pubkey,
    pub bond_amount: u64,
    pub region: Region,
    pub rpc_endpoint: [u8; 64],
    pub shreds_munched: u64,
    pub bad_cleanups: u32,
    pub active: bool,
    pub registered_epoch: u64,
}
impl MuncherNode { pub const LEN: usize = 32 + 8 + 1 + 64 + 8 + 4 + 1 + 8; }

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy)]
pub struct ShredLog {
    pub shred_type: ShredType,
    pub severity: Severity,
    pub original_sig: [u8; 64],
    pub resolution: Resolution,
    pub muncher_node: Pubkey,
    pub slot_detected: u64,
    pub affected_validators: u8,
    pub logged_slot: u64,
    pub disputed: bool,
}
impl ShredLog { pub const LEN: usize = 1 + 1 + 64 + 1 + 32 + 8 + 1 + 8 + 1; }

// ── EVENTS ────────────────────────────────────────────────────────────────────

#[event]
pub struct MuncherRegistered { pub operator: Pubkey, pub bond: u64, pub region: Region, pub epoch: u64 }
#[event]
pub struct ShredMunched { pub shred_type: ShredType, pub severity: Severity, pub resolution: Resolution, pub muncher: Pubkey, pub slot_detected: u64, pub affected_validators: u8, pub fee: u64, pub burned: u64, pub slot: u64 }
#[event]
pub struct Subscribed { pub subscriber: Pubkey, pub expires_epoch: u64, pub fee: u64, pub burned: u64, pub epoch: u64 }
#[event]
pub struct CleanupDisputed { pub shred_log_index: u32, pub challenger: Pubkey, pub disputed_muncher: Pubkey, pub slot: u64 }
#[event]
pub struct MuncherSlashed { pub operator: Pubkey, pub slash_amount: u64, pub burned: u64, pub epoch: u64 }

// ── ERRORS ────────────────────────────────────────────────────────────────────

#[error_code]
pub enum MuncherError {
    #[msg("Bond below minimum (500 XNT)")]
    BondTooSmall,
    #[msg("Too many muncher nodes")]
    TooManyMunchers,
    #[msg("Already registered")]
    AlreadyRegistered,
    #[msg("Not a registered muncher")]
    NotAMuncher,
    #[msg("Muncher not found")]
    MuncherNotFound,
    #[msg("Shred log entry not found")]
    LogNotFound,
    #[msg("Already disputed")]
    AlreadyDisputed,
    #[msg("Unauthorized")]
    Unauthorized,
    #[msg("Math overflow")]
    MathOverflow,
    #[msg("Invalid treasury")]
    InvalidTreasury,
    #[msg("Invalid burn address")]
    InvalidBurnAddress,
}
