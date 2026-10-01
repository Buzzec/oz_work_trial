#![allow(dead_code)]
//! Runtime fixtures shared by RFQ scenarios; every assertion replays the actual host CPI circuit.

use anchor_lang::{Discriminator, prelude::*};
use confidential_rfq::{
    accounts, instruction,
    state::{
        market::Market,
        rfq::{RFQ, RFQPrivateField},
    },
};
use confidential_token as ct;
use mollusk_svm::{
    Mollusk,
    result::{Check, InstructionResult},
};
use solana_sdk::{account::Account as SolanaAccount, instruction::Instruction};
use std::collections::HashMap;
use zama_solana_test_kit::{
    self as kit, Ctx,
    oracle::{CleartextLedger, TypedClearValue},
    signing::amount_attestation_for,
};

pub const NOW: u64 = 1_000;
pub const DEADLINE: u64 = 1_100;
const TOKEN: Pubkey = solana_sdk::pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
const ATA: Pubkey = solana_sdk::pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");

pub struct TokenFixture {
    pub mint: Pubkey,
    pub underlying: Pubkey,
}
impl TokenFixture {
    pub fn token(&self, owner: Pubkey) -> Pubkey {
        ct::token_account_address(self.mint, owner).0
    }
    pub fn store(&self, owner: Pubkey) -> Pubkey {
        ct::encrypted_store_address(self.mint, self.token(owner)).0
    }
    pub fn ata(&self, owner: Pubkey) -> Pubkey {
        Pubkey::find_program_address(
            &[owner.as_ref(), TOKEN.as_ref(), self.underlying.as_ref()],
            &ATA,
        )
        .0
    }
    pub fn side(&self, owner: Pubkey, rfq: Pubkey) -> accounts::TokenSide {
        accounts::TokenSide {
            confidential_mint: self.mint,
            underlying_mint: self.underlying,
            participant_ata: self.ata(owner),
            rfq_ata: self.ata(rfq),
            participant_token_account: self.token(owner),
            rfq_token_account: self.token(rfq),
            participant_balance_store: self.store(owner),
            rfq_balance_store: self.store(rfq),
        }
    }
}

pub struct RfqFixture {
    pub context: Ctx,
    pub ledger: CleartextLedger,
    pub user: Pubkey,
    pub makers: Vec<Pubkey>,
    pub market: Pubkey,
    pub rfq: Pubkey,
    pub rfq_store: Pubkey,
    pub rfq_funder: Pubkey,
    pub asset: TokenFixture,
    pub basis: TokenFixture,
    pub host_config: Pubkey,
    pub request: Instruction,
    pub funding: Instruction,
    next_handle: u8,
}

impl RfqFixture {
    /// Start with 1,000 units of each token per participant and an unopened RFQ.
    pub fn new(buyer: bool, size: u64, limit: u64, deadline: u64) -> Self {
        let user = Pubkey::new_unique();
        let makers = vec![
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
        ];
        let market = Pubkey::new_unique();
        let asset = TokenFixture {
            mint: Pubkey::new_unique(),
            underlying: Pubkey::new_unique(),
        };
        let basis = TokenFixture {
            mint: Pubkey::new_unique(),
            underlying: Pubkey::new_unique(),
        };
        let amounts = kit::handle_for_chain(1, 6);
        let buyer_handle = kit::handle_for_chain(2, 0);
        let expiry_handle = kit::handle_for_chain(3, 5);
        let asset_handle = kit::handle_for_chain(4, 5);
        let basis_handle = kit::handle_for_chain(5, 5);
        let rfq = Pubkey::find_program_address(
            &[b"rfq", market.as_ref(), user.as_ref(), &amounts],
            &confidential_rfq::ID,
        )
        .0;
        let rfq_store = zama_host::encrypted_store_address(confidential_rfq::ID, rfq, amounts).0;
        let rfq_funder = confidential_rfq::util::pda::rfq_funder_address(rfq).0;
        let group_bump = Pubkey::find_program_address(
            &[b"market_maker_group", market.as_ref()],
            &confidential_rfq::ID,
        )
        .1;
        let mut market_data = Market::new(user, group_bump);
        for (i, maker) in makers.iter().enumerate() {
            market_data.add_maker(i as u32 + 1, *maker).unwrap();
        }
        let mut host_params = kit::HostConfigParams::new(user);
        host_params.coprocessor_signers = vec![kit::signing::secp_evm_address(
            &kit::signing::coprocessor_signing_key(),
        )];
        let (host_config, host_account) = kit::host_config_account(&host_params);
        let mut entries = HashMap::from([
            (user, kit::funded_system_account()),
            (
                market,
                account(confidential_rfq::ID, kit::serialized_account(market_data)),
            ),
            (host_config, host_account),
            (rfq, kit::empty_system_account()),
            (rfq_store, kit::empty_system_account()),
            (rfq_funder, kit::empty_system_account()),
            (
                kit::event_authority(zama_host::ID),
                kit::empty_system_account(),
            ),
            (kit::event_authority(ct::ID), kit::empty_system_account()),
        ]);
        for maker in &makers {
            entries.insert(*maker, kit::funded_system_account());
        }
        let mut ledger = CleartextLedger::default();
        let mut seed = 10;
        for side in [&asset, &basis] {
            entries.insert(
                side.mint,
                account(
                    ct::ID,
                    kit::serialized_account(ct::ConfidentialMint {
                        authority: user,
                        underlying_mint: side.underlying,
                        decimals: 6,
                    }),
                ),
            );
            entries.insert(side.underlying, kit::spl_mint_account(None, 0));
            for owner in std::iter::once(user).chain(makers.iter().copied()) {
                let handle = kit::handle_for_chain(seed, 5);
                seed += 1;
                let token = side.token(owner);
                entries.insert(
                    token,
                    account(
                        ct::ID,
                        kit::serialized_account(ct::ConfidentialTokenAccount {
                            owner,
                            mint: side.mint,
                            bump: ct::token_account_address(side.mint, owner).1,
                        }),
                    ),
                );
                let (store, data) = kit::new_encrypted_store(
                    ct::token_app(side.mint),
                    token,
                    [(ct::balance_key(), handle)],
                );
                entries.insert(store, kit::encrypted_store_account(&data));
                entries.insert(side.ata(owner), kit::empty_system_account());
                ledger.seed_amount(handle, 1_000);
            }
            entries.insert(side.token(rfq), kit::empty_system_account());
            entries.insert(side.store(rfq), kit::empty_system_account());
            entries.insert(side.ata(rfq), kit::empty_system_account());
        }
        let mut packed = [0; 32];
        packed[16..24].copy_from_slice(&size.to_be_bytes());
        packed[24..].copy_from_slice(&limit.to_be_bytes());
        ledger
            .values
            .insert(amounts, TypedClearValue::from_be_bytes(6, packed));
        ledger
            .values
            .insert(buyer_handle, TypedClearValue::from_u64(0, buyer as u64));
        ledger.seed_amount(expiry_handle, deadline);
        ledger.seed_amount(asset_handle, if buyer { 0 } else { size });
        ledger.seed_amount(basis_handle, if buyer { limit } else { 0 });
        let request = kit::anchor_ix(
            confidential_rfq::ID,
            accounts::RequestQuote {
                user,
                market,
                rfq,
                rfq_funder,
                rfq_store,
                asset: asset.side(user, rfq),
                basis: basis.side(user, rfq),
                host_config,
                zama_event_authority: kit::event_authority(zama_host::ID),
                transient_store: zama_host::transient_store_address(user).0,
                instructions: solana_sdk::sysvar::instructions::ID,
                zama_program: zama_host::ID,
                confidential_token_event_authority: kit::event_authority(ct::ID),
                confidential_token_program: ct::ID,
                system_program: System::id(),
            },
            instruction::RequestQuote {
                amounts: amount_attestation_for(amounts, user, confidential_rfq::ID).into(),
                user_buyer: amount_attestation_for(buyer_handle, user, confidential_rfq::ID).into(),
                timeout: amount_attestation_for(expiry_handle, user, confidential_rfq::ID).into(),
                bid_capacity: 3,
            },
        );
        let funding = kit::anchor_ix(
            confidential_rfq::ID,
            accounts::FundQuote {
                user,
                rfq,
                rfq_store,
                asset: asset.side(user, rfq),
                basis: basis.side(user, rfq),
                host_config,
                zama_event_authority: kit::event_authority(zama_host::ID),
                transient_store: zama_host::transient_store_address(user).0,
                instructions: solana_sdk::sysvar::instructions::ID,
                zama_program: zama_host::ID,
                confidential_token_event_authority: kit::event_authority(ct::ID),
                confidential_token_program: ct::ID,
                system_program: System::id(),
            },
            instruction::FundQuote {
                asset_escrow: amount_attestation_for(asset_handle, user, ct::ID).into(),
                basis_escrow: amount_attestation_for(basis_handle, user, ct::ID).into(),
            },
        );
        let deploy = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/deploy/");
        let mut svm = Mollusk::new(&confidential_rfq::ID, &format!("{deploy}confidential_rfq"));
        svm.add_program(&zama_host::ID, &format!("{deploy}zama_host"));
        svm.add_program(&ct::ID, &format!("{deploy}confidential_token"));
        kit::set_previous_bank_hash_sysvars(&mut svm);
        svm.sysvars.clock.unix_timestamp = NOW as i64;
        svm.compute_budget.compute_unit_limit = 1_400_000;
        let context = svm.with_context(entries);
        Self {
            context,
            ledger,
            user,
            makers,
            market,
            rfq,
            rfq_store,
            rfq_funder,
            asset,
            basis,
            host_config,
            request,
            funding,
            next_handle: 30,
        }
    }

    /// Prepare each permissionless escrow using the real token/host programs while
    /// the RFQ is still absent. Each independent transaction keeps the 1.4M budget.
    pub fn prepare_escrows(&mut self) {
        for asset in [true, false] {
            let side = if asset { &self.asset } else { &self.basis };
            if self
                .context
                .account_store
                .borrow()
                .get(&side.token(self.rfq))
                .is_some_and(|account| !account.data.is_empty())
            {
                continue;
            }
            let ix = kit::anchor_ix(
                ct::ID,
                ct::accounts::InitializeTokenAccount {
                    payer: self.user,
                    owner: self.rfq,
                    mint: side.mint,
                    token_account: side.token(self.rfq),
                    balance_encrypted_store: side.store(self.rfq),
                    zama_event_authority: kit::event_authority(zama_host::ID),
                    transient_store: zama_host::transient_store_address(self.user).0,
                    instructions: solana_sdk::sysvar::instructions::ID,
                    zama_program: zama_host::ID,
                    host_config: self.host_config,
                    system_program: System::id(),
                    hcu_block_meter: None,
                    hcu_trusted_app_record: None,
                    event_authority: kit::event_authority(ct::ID),
                    program: ct::ID,
                },
                ct::instruction::InitializeTokenAccount {},
            );
            self.process(ix);
        }
    }

    pub fn open(&mut self) {
        self.create();
        self.fund();
    }
    pub fn create(&mut self) {
        self.process(self.request.clone());
    }
    pub fn fund(&mut self) {
        self.process(self.funding.clone());
    }
    pub fn process(&mut self, ix: Instruction) -> InstructionResult {
        let result = kit::transaction::process_fhe_instruction(
            &self.context,
            self.user,
            &ix,
            &[Check::success()],
        );
        self.ledger.replay_fhe_cpis(&self.context, &result);
        result
    }
    pub fn value(&self, field: RFQPrivateField) -> u64 {
        self.store_value(self.rfq_store, field.key())
    }
    pub fn store_value(&self, store: Pubkey, key: [u8; 32]) -> u64 {
        let handle = kit::read_store_handle(&self.context, store, key);
        let value = self.ledger.values[&handle].value;
        u64::from_be_bytes(value[24..].try_into().unwrap())
    }
    pub fn balance(&self, asset: bool, owner: Pubkey) -> u64 {
        let side = if asset { &self.asset } else { &self.basis };
        self.ledger
            .u64_in_state(&self.context, side.store(owner), ct::balance_key())
    }
    pub fn state(&self) -> RFQ {
        let data = self
            .context
            .account_store
            .borrow()
            .get(&self.rfq)
            .unwrap()
            .data
            .clone();
        bytemuck::pod_read_unaligned(&data[RFQ::DISCRIMINATOR.len()..])
    }
    /// Build lifecycle instructions with fixed beneficiaries; normal user claims are relayed.
    pub fn expire(&self) -> Instruction {
        kit::anchor_ix(
            confidential_rfq::ID,
            accounts::ExpireRfq {
                caller: self.user,
                market: self.market,
                rfq: self.rfq,
                rfq_store: self.rfq_store,
                host_config: self.host_config,
                zama_event_authority: kit::event_authority(zama_host::ID),
                transient_store: zama_host::transient_store_address(self.user).0,
                instructions: solana_sdk::sysvar::instructions::ID,
                zama_program: zama_host::ID,
                system_program: System::id(),
            },
            instruction::ExpireRfq { maker_id: None },
        )
    }

    pub fn maker_store(&self, maker_id: u32) -> (Pubkey, Pubkey) {
        let authority = confidential_rfq::util::pda::maker_store_address(self.rfq, maker_id).0;
        let store =
            zama_host::encrypted_store_address(confidential_rfq::ID, authority, self.state().nonce)
                .0;
        (authority, store)
    }

    pub fn scan(&self, maker_id: u32) -> Instruction {
        let (maker_store_authority, maker_store) = self.maker_store(maker_id);
        kit::anchor_ix(
            confidential_rfq::ID,
            accounts::CalculateWinner {
                caller: self.user,
                rfq: self.rfq,
                rfq_store: self.rfq_store,
                maker_store_authority,
                maker_store,
                host_config: self.host_config,
                zama_event_authority: kit::event_authority(zama_host::ID),
                transient_store: zama_host::transient_store_address(self.user).0,
                instructions: solana_sdk::sysvar::instructions::ID,
                zama_program: zama_host::ID,
                system_program: System::id(),
            },
            instruction::CalculateWinner { maker_id },
        )
    }

    pub fn user_claim(&self, cancel: bool) -> Instruction {
        let accounts = accounts::ClaimRfqUser {
            // A different signer submits the normal claim; the beneficiary remains the user.
            caller: if cancel { self.user } else { self.makers[2] },
            user: self.user,
            rfq: self.rfq,
            rfq_store: self.rfq_store,
            asset: self.asset.side(self.user, self.rfq),
            basis: self.basis.side(self.user, self.rfq),
            host_config: self.host_config,
            zama_event_authority: kit::event_authority(zama_host::ID),
            transient_store: zama_host::transient_store_address(self.user).0,
            instructions: solana_sdk::sysvar::instructions::ID,
            zama_program: zama_host::ID,
            confidential_token_event_authority: kit::event_authority(confidential_token::ID),
            confidential_token_program: confidential_token::ID,
            system_program: System::id(),
        };
        if cancel {
            kit::anchor_ix(confidential_rfq::ID, accounts, instruction::CancelQuote {})
        } else {
            kit::anchor_ix(confidential_rfq::ID, accounts, instruction::ClaimRfqUser {})
        }
    }

    pub fn maker_claim(&self, maker_id: u32) -> Instruction {
        let maker = self.makers[maker_id as usize - 1];
        let (maker_store_authority, maker_store) = self.maker_store(maker_id);
        kit::anchor_ix(
            confidential_rfq::ID,
            accounts::ClaimRfqMaker {
                caller: self.user,
                maker,
                market: self.market,
                rfq: self.rfq,
                rfq_store: self.rfq_store,
                maker_store_authority,
                maker_store,
                asset: self.asset.side(maker, self.rfq),
                basis: self.basis.side(maker, self.rfq),
                host_config: self.host_config,
                zama_event_authority: kit::event_authority(zama_host::ID),
                transient_store: zama_host::transient_store_address(self.user).0,
                instructions: solana_sdk::sysvar::instructions::ID,
                zama_program: zama_host::ID,
                confidential_token_event_authority: kit::event_authority(confidential_token::ID),
                confidential_token_program: confidential_token::ID,
                system_program: System::id(),
            },
            instruction::ClaimRfqMaker { maker_id },
        )
    }

    pub fn maker_value(
        &self,
        maker_id: u32,
        field: confidential_rfq::state::rfq::MakerPrivateField,
    ) -> u64 {
        self.store_value(self.maker_store(maker_id).1, field.key())
    }
    pub fn set_clock(&mut self, now: u64) {
        self.context.mollusk.sysvars.clock.unix_timestamp = now as i64;
    }
    pub fn bid(&mut self, maker_id: u32, buy: u64, sell: u64) -> Instruction {
        let maker_store = zama_host::encrypted_store_address(
            confidential_rfq::ID,
            confidential_rfq::util::pda::maker_store_address(self.rfq, maker_id).0,
            self.state().nonce,
        )
        .0;
        let (old_buy, old_sell) = if self
            .context
            .account_store
            .borrow()
            .get(&maker_store)
            .is_some_and(|account| !account.data.is_empty())
        {
            use confidential_rfq::state::rfq::MakerPrivateField;
            (
                self.store_value(maker_store, MakerPrivateField::Buy.key()),
                self.store_value(maker_store, MakerPrivateField::Sell.key()),
            )
        } else {
            (0, 0)
        };
        let asset_deposit = if sell != 0 && old_sell == 0 {
            self.value(RFQPrivateField::Size)
        } else {
            0
        };
        self.bid_with_deposits(
            maker_id,
            buy,
            sell,
            asset_deposit,
            buy.saturating_sub(old_buy),
        )
    }
    pub fn bid_with_deposits(
        &mut self,
        maker_id: u32,
        buy: u64,
        sell: u64,
        asset_deposit: u64,
        basis_deposit: u64,
    ) -> Instruction {
        let maker = self.makers[maker_id as usize - 1];
        let maker_authority =
            confidential_rfq::util::pda::maker_store_address(self.rfq, maker_id).0;
        let maker_store = zama_host::encrypted_store_address(
            confidential_rfq::ID,
            maker_authority,
            self.state().nonce,
        )
        .0;
        for key in [maker_authority, maker_store] {
            self.context
                .account_store
                .borrow_mut()
                .entry(key)
                .or_insert_with(kit::empty_system_account);
        }
        let prices = self.prices(maker_id, buy, sell);
        let asset_transfer_attestation = self.deposit_attestation(maker, asset_deposit);
        let basis_transfer_attestation = self.deposit_attestation(maker, basis_deposit);
        kit::anchor_ix(
            confidential_rfq::ID,
            accounts::PlaceBid {
                maker,
                market: self.market,
                rfq: self.rfq,
                rfq_funder: self.rfq_funder,
                rfq_store: self.rfq_store,
                maker_authority,
                maker_store,
                asset: self.asset.side(maker, self.rfq),
                basis: self.basis.side(maker, self.rfq),
                confidential_token_event_authority: kit::event_authority(ct::ID),
                confidential_token_program: ct::ID,
                zama_event_authority: kit::event_authority(zama_host::ID),
                transient_store: zama_host::transient_store_address(self.user).0,
                instructions: solana_sdk::sysvar::instructions::ID,
                host_config: self.host_config,
                zama_program: zama_host::ID,
                system_program: System::id(),
                hcu_block_meter: None,
                hcu_trusted_app_record: None,
            },
            instruction::PlaceBid {
                maker_id,
                prices: prices.into(),
                asset_transfer_attestation: asset_transfer_attestation.into(),
                basis_transfer_attestation: basis_transfer_attestation.into(),
            },
        )
    }
    fn deposit_attestation(
        &mut self,
        maker: Pubkey,
        amount: u64,
    ) -> zama_host::CoprocessorInputAttestation {
        let handle = kit::handle_for_chain(self.next_handle, 5);
        self.next_handle += 1;
        self.ledger.seed_amount(handle, amount);
        amount_attestation_for(handle, maker, ct::ID)
    }
    pub fn prices(
        &mut self,
        maker_id: u32,
        buy: u64,
        sell: u64,
    ) -> zama_host::CoprocessorInputAttestation {
        let handle = kit::handle_for_chain(self.next_handle, 6);
        self.next_handle += 1;
        let mut packed = [0; 32];
        packed[16..24].copy_from_slice(&buy.to_be_bytes());
        packed[24..].copy_from_slice(&sell.to_be_bytes());
        self.ledger
            .values
            .insert(handle, TypedClearValue::from_be_bytes(6, packed));
        amount_attestation_for(
            handle,
            self.makers[maker_id as usize - 1],
            confidential_rfq::ID,
        )
    }
}
fn account(owner: Pubkey, data: Vec<u8>) -> SolanaAccount {
    SolanaAccount {
        lamports: Rent::default().minimum_balance(data.len()),
        data,
        owner,
        executable: false,
        rent_epoch: 0,
    }
}
