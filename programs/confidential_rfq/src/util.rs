use anchor_lang::prelude::*;
use zama_fhe::ExecutionCpiAccounts;
use zama_host::program::ZamaHost;

pub trait Contains<T> {
    fn value(&self) -> &T;
}
impl<T> Contains<T> for T {
    fn value(&self) -> &T {
        self
    }
}
impl<'info> Contains<Program<'info, System>> for ExecutionPassthroughAccounts<'info> {
    fn value(&self) -> &Program<'info, System> {
        &self.system_program
    }
}
impl<'info> Contains<Program<'info, ZamaHost>> for ExecutionPassthroughAccounts<'info> {
    fn value(&self) -> &Program<'info, ZamaHost> {
        &self.zama_program
    }
}

#[derive(Accounts)]
pub struct ExecutionPassthroughAccounts<'info> {
    /// CHECK: canonical host config is validated by ZamaHost.
    pub host_config: UncheckedAccount<'info>,
    /// CHECK: host event-CPI authority is validated by ZamaHost.
    pub zama_event_authority: UncheckedAccount<'info>,
    /// CHECK: shared transaction journal, validated by ZamaHost.
    #[account(mut)]
    pub transient_store: UncheckedAccount<'info>,
    /// CHECK: the host validates the Instructions sysvar and the final close instruction.
    pub instructions: UncheckedAccount<'info>,
    pub zama_program: Program<'info, ZamaHost>,
    pub system_program: Program<'info, System>,
    /// CHECK: optional per-application meter, validated by ZamaHost when metering is enabled.
    #[account(mut)]
    pub hcu_block_meter: Option<UncheckedAccount<'info>>,
    /// CHECK: optional host-admin trust record, validated by ZamaHost.
    pub hcu_trusted_app_record: Option<UncheckedAccount<'info>>,
}
impl<'info> ExecutionPassthroughAccounts<'info> {
    pub fn to_cpi_accounts<'a, A: 'a + ToAccountInfo<'info>>(
        &self,
        owner: &impl ToAccountInfo<'info>,
        authority: &impl ToAccountInfo<'info>,
        remaining_accounts: impl IntoIterator<Item = &'a A>,
    ) -> ExecutionCpiAccounts<'info> {
        ExecutionCpiAccounts {
            payer: owner.to_account_info(),
            authority: authority.to_account_info(),
            host_config: self.host_config.to_account_info(),
            deny_scope_records: remaining_accounts
                .into_iter()
                .map(ToAccountInfo::to_account_info)
                .collect(),
            system_program: self.system_program.to_account_info(),
            hcu_block_meter: self
                .hcu_block_meter
                .as_ref()
                .map(ToAccountInfo::to_account_info),
            hcu_trusted_app_record: self
                .hcu_trusted_app_record
                .as_ref()
                .map(ToAccountInfo::to_account_info),
            rand_nonce: None,
            event_authority: self.zama_event_authority.to_account_info(),
            transient_store: self.transient_store.to_account_info(),
            instructions: self.instructions.to_account_info(),
            program: self.zama_program.to_account_info(),
        }
    }
}
