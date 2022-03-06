// Copyright 2021 Parallel Finance Developer.
// This file is part of Parallel Finance.

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
// http://www.apache.org/licenses/LICENSE-2.0

// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! # Liquid staking pallet
//!
//! ## Overview
//!
//! This pallet manages the NPoS operations for relay chain asset.

// TODO: multi-accounts support
// TODO: fix benchmarks
// TODO: fix unit tests
// TODO: enrich unit tests and try to find a way run relaychain block to target block
// TODO: overflow of matchingpool

#![cfg_attr(not(feature = "std"), no_std)]

mod benchmarking;

#[cfg(test)]
mod mock;
#[cfg(test)]
mod tests;

pub mod types;
pub mod weights;

#[macro_use]
extern crate primitives;

use frame_support::traits::{fungibles::InspectMetadata, tokens::Balance as BalanceT, Get};
use primitives::{
    ExchangeRateProvider, LiquidStakingConvert, LiquidStakingCurrenciesProvider, Rate,
};
use sp_runtime::{traits::Zero, FixedPointNumber, FixedPointOperand};

pub use pallet::*;

#[frame_support::pallet]
pub mod pallet {
    use frame_support::{
        dispatch::{DispatchResult, DispatchResultWithPostInfo},
        ensure,
        error::BadOrigin,
        log,
        pallet_prelude::*,
        require_transactional,
        storage::with_transaction,
        traits::{
            fungibles::{Inspect, InspectMetadata, Mutate, Transfer},
            IsType, SortedMembers,
        },
        transactional, PalletId,
    };
    use frame_system::{
        ensure_signed,
        pallet_prelude::{BlockNumberFor, OriginFor},
    };
    use pallet_xcm::ensure_response;
    use sp_runtime::{
        traits::{AccountIdConversion, BlockNumberProvider, CheckedDiv, CheckedSub, StaticLookup},
        ArithmeticError, FixedPointNumber, TransactionOutcome,
    };
    use sp_std::{boxed::Box, result::Result, vec::Vec};

    use primitives::{
        ump::*, ArithmeticKind, Balance, CurrencyId, DerivativeIndex, EraIndex,
        LiquidStakingConvert, ParaId, Rate, Ratio,
    };

    use super::{types::*, weights::WeightInfo, *};
    use pallet_xcm_helper::XcmHelper;
    use xcm::latest::prelude::*;

    pub const MAX_UNLOCKING_CHUNKS: usize = 32;

    pub type AccountIdOf<T> = <T as frame_system::Config>::AccountId;
    pub type AssetIdOf<T> =
        <<T as Config>::Assets as Inspect<<T as frame_system::Config>::AccountId>>::AssetId;
    pub type BalanceOf<T> =
        <<T as Config>::Assets as Inspect<<T as frame_system::Config>::AccountId>>::Balance;

    #[pallet::pallet]
    #[pallet::generate_store(pub(super) trait Store)]
    #[pallet::without_storage_info]
    pub struct Pallet<T>(_);

    #[pallet::config]
    pub trait Config: frame_system::Config + pallet_utility::Config + pallet_xcm::Config {
        type Event: From<Event<Self>> + IsType<<Self as frame_system::Config>::Event>;

        type Origin: IsType<<Self as frame_system::Config>::Origin>
            + Into<Result<pallet_xcm::Origin, <Self as Config>::Origin>>;

        type Call: IsType<<Self as pallet_xcm::Config>::Call> + From<Call<Self>>;

        /// Assets for deposit/withdraw assets to/from pallet account
        type Assets: Transfer<Self::AccountId, AssetId = CurrencyId>
            + Mutate<Self::AccountId, AssetId = CurrencyId, Balance = Balance>
            + InspectMetadata<Self::AccountId, AssetId = CurrencyId, Balance = Balance>;

        /// The origin which can do operation on relaychain using parachain's sovereign account
        type RelayOrigin: EnsureOrigin<<Self as frame_system::Config>::Origin>;

        /// The origin which can update liquid currency, staking currency and other parameters
        type UpdateOrigin: EnsureOrigin<<Self as frame_system::Config>::Origin>;

        /// Approved accouts which can call `withdraw_unbonded` and `settlement`
        type Members: SortedMembers<Self::AccountId>;

        /// The pallet id of liquid staking, keeps all the staking assets
        #[pallet::constant]
        type PalletId: Get<PalletId>;

        /// Returns the parachain ID we are running with.
        #[pallet::constant]
        type SelfParaId: Get<ParaId>;

        /// Derivative index
        #[pallet::constant]
        type DerivativeIndex: Get<DerivativeIndex>;

        /// Xcm fees
        #[pallet::constant]
        type XcmFees: Get<BalanceOf<Self>>;

        /// Staking currency
        #[pallet::constant]
        type StakingCurrency: Get<AssetIdOf<Self>>;

        /// Liquid currency
        #[pallet::constant]
        type LiquidCurrency: Get<AssetIdOf<Self>>;

        /// Minimum stake amount
        #[pallet::constant]
        type MinStake: Get<BalanceOf<Self>>;

        /// Minimum unstake amount
        #[pallet::constant]
        type MinUnstake: Get<BalanceOf<Self>>;

        /// Weight information
        type WeightInfo: WeightInfo;

        /// Number of unbond indexes for unlocking.
        #[pallet::constant]
        type BondingDuration: Get<EraIndex>;

        /// Number of blocknumbers that each period contains.
        /// SessionsPerEra * EpochDuration / MILLISECS_PER_BLOCK
        #[pallet::constant]
        type EraLength: Get<BlockNumberFor<Self>>;

        /// The relay's BlockNumber provider
        type RelayChainBlockNumberProvider: BlockNumberProvider<BlockNumber = BlockNumberFor<Self>>;

        /// To expose XCM helper functions
        type XCM: XcmHelper<Self, BalanceOf<Self>, AssetIdOf<Self>, Self::AccountId>;
    }

    #[pallet::event]
    #[pallet::generate_deposit(pub(super) fn deposit_event)]
    pub enum Event<T: Config> {
        /// The assets get staked successfully
        Staked(T::AccountId, BalanceOf<T>),
        /// The derivative get unstaked successfully
        Unstaked(T::AccountId, BalanceOf<T>, BalanceOf<T>),
        /// Staking ledger feeded
        StakingLedgerUpdated(StakingLedger<T::AccountId, BalanceOf<T>>),
        /// Sent staking.bond call to relaychain
        Bonding(T::AccountId, BalanceOf<T>, RewardDestination<T::AccountId>),
        /// Sent staking.bond_extra call to relaychain
        BondingExtra(BalanceOf<T>),
        /// Sent staking.unbond call to relaychain
        Unbonding(BalanceOf<T>),
        /// Sent staking.rebond call to relaychain
        Rebonding(BalanceOf<T>),
        /// Sent staking.withdraw_unbonded call to relaychain
        WithdrawingUnbonded(u32),
        /// Sent staking.nominate call to relaychain
        Nominating(Vec<T::AccountId>),
        /// Liquid currency's market cap was updated
        MarketCapUpdated(BalanceOf<T>),
        /// InsurancePool's reserve_factor was updated
        ReserveFactorUpdated(Ratio),
        /// Exchange rate was updated
        ExchangeRateUpdated(Rate),
        /// Notification received
        /// [multi_location, query_id, res]
        NotificationReceived(Box<MultiLocation>, QueryId, Option<(u32, XcmError)>),
        /// Claim user's unbonded staking assets
        /// [era_index, account_id, amount]
        ClaimedFor(EraIndex, T::AccountId, BalanceOf<T>),
    }

    #[pallet::error]
    pub enum Error<T> {
        /// Exchange rate is invalid.
        InvalidExchangeRate,
        /// The stake was below the minimum, `MinStake`.
        StakeTooSmall,
        /// The unstake was below the minimum, `MinUnstake`.
        UnstakeTooSmall,
        /// Invalid liquid currency
        InvalidLiquidCurrency,
        /// Invalid staking currency
        InvalidStakingCurrency,
        /// Exceeded liquid currency's market cap
        CapExceeded,
        /// Invalid market cap
        InvalidCap,
        /// The factor should be bigger than 0% and smaller than 100%
        InvalidFactor,
        /// Nothing to claim yet
        NothingToClaim,
        /// Stash wasn't bonded yet
        NotBonded,
        /// Stash is already bonded.
        AlreadyBonded,
        /// Can not schedule more unlock chunks.
        NoMoreChunks,
    }

    /// The exchange rate between relaychain native asset and the voucher.
    #[pallet::storage]
    #[pallet::getter(fn exchange_rate)]
    pub type ExchangeRate<T: Config> = StorageValue<_, Rate, ValueQuery>;

    /// Fraction of reward currently set aside for reserves.
    #[pallet::storage]
    #[pallet::getter(fn reserve_factor)]
    pub type ReserveFactor<T: Config> = StorageValue<_, Ratio, ValueQuery>;

    #[pallet::storage]
    #[pallet::getter(fn total_reserves)]
    pub type TotalReserves<T: Config> = StorageValue<_, BalanceOf<T>, ValueQuery>;

    /// Store total stake amount and unstake amount in each era,
    /// And will update when stake/unstake occurred.
    #[pallet::storage]
    #[pallet::getter(fn matching_pool)]
    pub type MatchingPool<T: Config> = StorageValue<_, MatchingLedger<BalanceOf<T>>, ValueQuery>;

    /// Liquid currency's market cap
    #[pallet::storage]
    #[pallet::getter(fn market_cap)]
    pub type MarketCap<T: Config> = StorageValue<_, BalanceOf<T>, ValueQuery>;

    /// Flying & failed xcm requests
    #[pallet::storage]
    #[pallet::getter(fn xcm_request)]
    pub type XcmRequests<T> = StorageMap<_, Blake2_128Concat, QueryId, XcmRequest<T>, OptionQuery>;

    /// Current era index
    /// Users can come to claim their unbonded staking assets back once this value arrived
    /// at certain height decided by `BondingDuration` and `EraLength`
    #[pallet::storage]
    #[pallet::getter(fn current_era)]
    pub type CurrentEra<T: Config> = StorageValue<_, EraIndex, ValueQuery>;

    /// Current era's start relaychain block
    #[pallet::storage]
    #[pallet::getter(fn era_start_block)]
    pub type EraStartBlock<T: Config> = StorageValue<_, BlockNumberFor<T>, ValueQuery>;

    /// Unbonding requests to be handled after arriving at target era
    #[pallet::storage]
    #[pallet::getter(fn unlockings)]
    pub type Unlockings<T: Config> =
        StorageMap<_, Blake2_128Concat, T::AccountId, Vec<UnlockChunk<BalanceOf<T>>>, OptionQuery>;

    /// Platform's staking ledgers
    #[pallet::storage]
    #[pallet::getter(fn staking_ledgers)]
    pub type StakingLedgers<T: Config> = StorageMap<
        _,
        Blake2_128Concat,
        DerivativeIndex,
        StakingLedger<T::AccountId, BalanceOf<T>>,
        OptionQuery,
    >;

    #[derive(Default)]
    #[pallet::genesis_config]
    pub struct GenesisConfig {
        pub exchange_rate: Rate,
        pub reserve_factor: Ratio,
    }

    #[pallet::genesis_build]
    impl<T: Config> GenesisBuild<T> for GenesisConfig {
        fn build(&self) {
            ExchangeRate::<T>::put(self.exchange_rate);
            ReserveFactor::<T>::put(self.reserve_factor);
        }
    }

    #[pallet::call]
    impl<T: Config> Pallet<T> {
        /// Put assets under staking, the native assets will be transferred to the account
        /// owned by the pallet, user receive derivative in return, such derivative can be
        /// further used as collateral for lending.
        ///
        /// - `amount`: the amount of staking assets
        #[pallet::weight(<T as Config>::WeightInfo::stake())]
        #[transactional]
        pub fn stake(
            origin: OriginFor<T>,
            #[pallet::compact] amount: BalanceOf<T>,
        ) -> DispatchResultWithPostInfo {
            let who = ensure_signed(origin)?;

            ensure!(amount >= T::MinStake::get(), Error::<T>::StakeTooSmall);

            let reserves = Self::reserve_factor().mul_floor(amount);

            let xcm_fees = T::XcmFees::get();
            let amount = amount
                .checked_sub(xcm_fees)
                .ok_or(ArithmeticError::Underflow)?;
            T::Assets::transfer(
                Self::staking_currency()?,
                &who,
                &Self::account_id(),
                amount,
                false,
            )?;
            T::XCM::add_xcm_fees(Self::staking_currency()?, &who, xcm_fees)?;

            let amount = amount
                .checked_sub(reserves)
                .ok_or(ArithmeticError::Underflow)?;
            let liquid_amount =
                Self::staking_to_liquid(amount).ok_or(Error::<T>::InvalidExchangeRate)?;
            let liquid_currency = Self::liquid_currency()?;
            Self::ensure_market_cap(liquid_currency, liquid_amount)?;

            T::Assets::mint_into(liquid_currency, &who, liquid_amount)?;

            log::trace!(
                target: "liquidStaking::stake",
                "stake_amount: {:?}, liquid_amount: {:?}, reserved: {:?}",
                &amount,
                &liquid_amount,
                &reserves
            );

            MatchingPool::<T>::try_mutate(|p| -> DispatchResult {
                p.update_total_stake_amount(amount, ArithmeticKind::Addition)
            })?;
            TotalReserves::<T>::try_mutate(|b| -> DispatchResult {
                *b = b.checked_add(reserves).ok_or(ArithmeticError::Overflow)?;
                Ok(())
            })?;

            Self::deposit_event(Event::<T>::Staked(who, amount));
            Ok(().into())
        }

        /// Unstake by exchange derivative for assets, the assets will not be avaliable immediately.
        /// Instead, the request is recorded and pending for the nomination accounts on relaychain
        /// chain to do the `unbond` operation.
        ///
        /// - `amount`: the amount of derivative
        #[pallet::weight(<T as Config>::WeightInfo::unstake())]
        #[transactional]
        pub fn unstake(
            origin: OriginFor<T>,
            #[pallet::compact] liquid_amount: BalanceOf<T>,
        ) -> DispatchResultWithPostInfo {
            let who = ensure_signed(origin)?;

            ensure!(
                liquid_amount >= T::MinUnstake::get(),
                Error::<T>::UnstakeTooSmall
            );

            let amount =
                Self::liquid_to_staking(liquid_amount).ok_or(Error::<T>::InvalidExchangeRate)?;

            Unlockings::<T>::try_mutate(&who, |b| -> DispatchResult {
                // TODO: check if we can bond before the next era
                // so that the one era's delay can be removed
                let mut chunks = b.take().unwrap_or_default();
                chunks.push(UnlockChunk {
                    value: amount,
                    era: Self::current_era() + T::BondingDuration::get() + 1,
                });
                ensure!(
                    chunks.len() <= MAX_UNLOCKING_CHUNKS,
                    Error::<T>::NoMoreChunks
                );
                *b = Some(chunks);
                Ok(())
            })?;

            T::Assets::burn_from(Self::liquid_currency()?, &who, liquid_amount)?;

            log::trace!(
                target: "liquidStaking::unstake",
                "unstake_amount: {:?}, liquid_amount: {:?}",
                &amount,
                &liquid_amount,
            );

            MatchingPool::<T>::try_mutate(|p| -> DispatchResult {
                p.update_total_unstake_amount(amount, ArithmeticKind::Addition)
            })?;

            Self::deposit_event(Event::<T>::Unstaked(who, liquid_amount, amount));
            Ok(().into())
        }

        /// Update insurance pool's reserve_factor
        #[pallet::weight(<T as Config>::WeightInfo::update_reserve_factor())]
        #[transactional]
        pub fn update_reserve_factor(
            origin: OriginFor<T>,
            reserve_factor: Ratio,
        ) -> DispatchResultWithPostInfo {
            T::UpdateOrigin::ensure_origin(origin)?;

            ensure!(
                reserve_factor > Ratio::zero() && reserve_factor < Ratio::one(),
                Error::<T>::InvalidFactor,
            );

            log::trace!(
                target: "liquidStaking::update_reserve_factor",
                 "reserve_factor: {:?}",
                &reserve_factor,
            );

            ReserveFactor::<T>::mutate(|v| *v = reserve_factor);
            Self::deposit_event(Event::<T>::ReserveFactorUpdated(reserve_factor));
            Ok(().into())
        }

        /// Update liquid currency's market cap
        /// stake will be blocked if passed liquid currency's market cap
        #[pallet::weight(<T as Config>::WeightInfo::update_market_cap())]
        #[transactional]
        pub fn update_market_cap(
            origin: OriginFor<T>,
            #[pallet::compact] cap: BalanceOf<T>,
        ) -> DispatchResultWithPostInfo {
            T::UpdateOrigin::ensure_origin(origin)?;

            ensure!(!cap.is_zero(), Error::<T>::InvalidCap);

            log::trace!(
                target: "liquidStaking::update_market_cap",
                "cap: {:?}",
                &cap,
            );
            MarketCap::<T>::mutate(|v| *v = cap);
            Self::deposit_event(Event::<T>::MarketCapUpdated(cap));
            Ok(().into())
        }

        /// feed staking_ledger for updating exchange rate.
        #[pallet::weight(<T as Config>::WeightInfo::update_staking_ledger())]
        #[transactional]
        pub fn update_staking_ledger(
            origin: OriginFor<T>,
            derivative_index: DerivativeIndex,
            staking_ledger: StakingLedger<T::AccountId, BalanceOf<T>>,
        ) -> DispatchResultWithPostInfo {
            Self::ensure_origin(origin)?;

            Self::do_update_exchange_rate(staking_ledger.active)?;
            Self::do_update_ledger(derivative_index, |ledger| {
                // TODO: validate staking_ledger using storage proof
                *ledger = staking_ledger.clone();
                Ok(())
            })?;

            log::trace!(
                target: "liquidStaking::update_staking_ledger",
                "staking_ledger: {:?}",
                &staking_ledger,
            );
            Self::deposit_event(Event::<T>::StakingLedgerUpdated(staking_ledger));
            Ok(().into())
        }

        /// Bond on relaychain via xcm.transact
        #[pallet::weight(<T as Config>::WeightInfo::bond())]
        #[transactional]
        pub fn bond(
            origin: OriginFor<T>,
            #[pallet::compact] amount: BalanceOf<T>,
            payee: RewardDestination<T::AccountId>,
        ) -> DispatchResult {
            T::RelayOrigin::ensure_origin(origin)?;
            Self::do_bond(amount, payee)?;
            Ok(())
        }

        /// Bond_extra on relaychain via xcm.transact
        #[pallet::weight(<T as Config>::WeightInfo::bond_extra())]
        #[transactional]
        pub fn bond_extra(
            origin: OriginFor<T>,
            #[pallet::compact] amount: BalanceOf<T>,
        ) -> DispatchResult {
            T::RelayOrigin::ensure_origin(origin)?;
            Self::do_bond_extra(amount)?;
            Ok(())
        }

        /// Unbond on relaychain via xcm.transact
        #[pallet::weight(<T as Config>::WeightInfo::unbond())]
        #[transactional]
        pub fn unbond(
            origin: OriginFor<T>,
            #[pallet::compact] amount: BalanceOf<T>,
        ) -> DispatchResult {
            T::RelayOrigin::ensure_origin(origin)?;
            Self::do_unbond(amount)?;
            Ok(())
        }

        /// Rebond on relaychain via xcm.transact
        #[pallet::weight(<T as Config>::WeightInfo::rebond())]
        #[transactional]
        pub fn rebond(
            origin: OriginFor<T>,
            #[pallet::compact] amount: BalanceOf<T>,
        ) -> DispatchResult {
            T::RelayOrigin::ensure_origin(origin)?;
            Self::do_rebond(amount)?;
            Ok(())
        }

        /// Withdraw unbonded on relaychain via xcm.transact
        #[pallet::weight(<T as Config>::WeightInfo::withdraw_unbonded())]
        #[transactional]
        pub fn withdraw_unbonded(origin: OriginFor<T>, num_slashing_spans: u32) -> DispatchResult {
            Self::ensure_origin(origin)?;
            Self::do_withdraw_unbonded(num_slashing_spans)?;
            Ok(())
        }

        /// Nominate on relaychain via xcm.transact
        #[pallet::weight(<T as Config>::WeightInfo::nominate())]
        #[transactional]
        pub fn nominate(origin: OriginFor<T>, targets: Vec<T::AccountId>) -> DispatchResult {
            Self::ensure_origin(origin)?;
            let query_id = T::XCM::do_nominate(
                targets.clone(),
                Self::staking_currency()?,
                T::DerivativeIndex::get(),
                Self::notify_placeholder(),
            )?;

            log::trace!(
                target: "liquidStaking::nominate",
                "targets: {:?}",
                &targets,
            );

            XcmRequests::<T>::insert(
                query_id,
                XcmRequest::Nominate {
                    targets: targets.clone(),
                },
            );
            Self::deposit_event(Event::<T>::Nominating(targets));
            Ok(())
        }

        /// Internal call which is expected to be triggered only by xcm instruction
        #[pallet::weight(<T as Config>::WeightInfo::notification_received())]
        #[transactional]
        pub fn notification_received(
            origin: OriginFor<T>,
            query_id: QueryId,
            response: Response,
        ) -> DispatchResultWithPostInfo {
            let responder = ensure_response(<T as Config>::Origin::from(origin))?;
            log::trace!(
                target: "liquidStaking::notification_received",
                "query_id: {:?}, response: {:?}",
                &query_id,
                &response
            );
            if let Response::ExecutionResult(res) = response {
                if let Some(request) = Self::xcm_request(&query_id) {
                    Self::do_notification_received(query_id, request, res)?;
                }

                Self::deposit_event(Event::<T>::NotificationReceived(
                    Box::new(responder),
                    query_id,
                    res,
                ));
            }
            Ok(().into())
        }

        /// Claim assets back when current era index arrived
        /// at target era
        #[pallet::weight(<T as Config>::WeightInfo::claim_for())]
        #[transactional]
        pub fn claim_for(
            origin: OriginFor<T>,
            dest: <T::Lookup as StaticLookup>::Source,
        ) -> DispatchResultWithPostInfo {
            Self::ensure_origin(origin)?;
            let who = T::Lookup::lookup(dest)?;
            let current_era = Self::current_era();

            Unlockings::<T>::try_mutate_exists(&who, |b| -> DispatchResult {
                let mut amount: BalanceOf<T> = Zero::zero();
                let chunks = b.as_mut().ok_or(Error::<T>::NothingToClaim)?;
                chunks.retain(|chunk| {
                    if chunk.era > current_era {
                        true
                    } else {
                        amount += chunk.value;
                        false
                    }
                });
                if amount.is_zero() {
                    return Err(Error::<T>::NothingToClaim.into());
                }
                if chunks.is_empty() {
                    *b = None;
                }
                T::Assets::transfer(
                    Self::staking_currency()?,
                    &Self::account_id(),
                    &who,
                    amount,
                    false,
                )?;

                log::trace!(
                    target: "liquidStaking::claim_for",
                    "current era: {:?}, beneficiary: {:?}, amount: {:?}",
                    &current_era,
                    &who,
                    amount
                );

                Self::deposit_event(Event::<T>::ClaimedFor(current_era, who.clone(), amount));
                Ok(())
            })?;
            Ok(().into())
        }
    }

    #[pallet::hooks]
    impl<T: Config> Hooks<T::BlockNumber> for Pallet<T> {
        fn on_initialize(_block_number: T::BlockNumber) -> u64 {
            with_transaction(|| {
                // TODO: fix weights and clean code
                let offset = Self::era_offset();
                let _ = Self::do_advance_era(offset);
                return TransactionOutcome::Commit(0);
            })
        }
    }

    impl<T: Config> Pallet<T> {
        /// Staking pool account
        pub fn account_id() -> T::AccountId {
            T::PalletId::get().into_account()
        }

        /// Parachain's sovereign account
        pub fn para_account_id() -> T::AccountId {
            T::SelfParaId::get().into_account()
        }

        /// Get staking currency or return back an error
        pub fn staking_currency() -> Result<AssetIdOf<T>, DispatchError> {
            Self::get_staking_currency()
                .ok_or(Error::<T>::InvalidStakingCurrency)
                .map_err(Into::into)
        }

        /// Get liquid currency or return back an error
        pub fn liquid_currency() -> Result<AssetIdOf<T>, DispatchError> {
            Self::get_liquid_currency()
                .ok_or(Error::<T>::InvalidLiquidCurrency)
                .map_err(Into::into)
        }

        /// Derivative parachain account
        pub fn derivative_para_account_id() -> T::AccountId {
            let para_account = Self::para_account_id();
            let derivative_index = T::DerivativeIndex::get();
            pallet_utility::Pallet::<T>::derivative_account_id(para_account, derivative_index)
        }

        // TODO: rename era offset to make it more clear
        fn era_offset() -> EraIndex {
            dbg!(T::RelayChainBlockNumberProvider::current_block_number());
            T::RelayChainBlockNumberProvider::current_block_number()
                .checked_sub(&Self::era_start_block())
                .and_then(|r| r.checked_div(&T::EraLength::get()))
                .and_then(|r| TryInto::<EraIndex>::try_into(r).ok())
                .unwrap_or_else(Zero::zero)
        }

        #[require_transactional]
        fn do_bond(amount: BalanceOf<T>, payee: RewardDestination<T::AccountId>) -> DispatchResult {
            if amount.is_zero() {
                return Ok(());
            }

            ensure!(
                !StakingLedgers::<T>::contains_key(&T::DerivativeIndex::get()),
                Error::<T>::AlreadyBonded
            );

            log::trace!(
                target: "liquidStaking::bond",
                "amount: {:?}",
                &amount,
            );

            let staking_currency = Self::staking_currency()?;
            let derivative_account_id = Self::derivative_para_account_id();
            let query_id = T::XCM::do_bond(
                amount,
                payee.clone(),
                derivative_account_id.clone(),
                staking_currency,
                T::DerivativeIndex::get(),
                Self::notify_placeholder(),
            )?;

            XcmRequests::<T>::insert(query_id, XcmRequest::Bond { amount });

            Self::deposit_event(Event::<T>::Bonding(derivative_account_id, amount, payee));

            Ok(())
        }

        #[require_transactional]
        fn do_bond_extra(amount: BalanceOf<T>) -> DispatchResult {
            if amount.is_zero() {
                return Ok(());
            }

            log::trace!(
                target: "liquidStaking::bond_extra",
                "amount: {:?}",
                &amount,
            );

            let query_id = T::XCM::do_bond_extra(
                amount,
                Self::derivative_para_account_id(),
                Self::staking_currency()?,
                T::DerivativeIndex::get(),
                Self::notify_placeholder(),
            )?;

            XcmRequests::<T>::insert(query_id, XcmRequest::BondExtra { amount });

            Self::deposit_event(Event::<T>::BondingExtra(amount));

            Ok(())
        }

        #[require_transactional]
        fn do_unbond(amount: BalanceOf<T>) -> DispatchResult {
            if amount.is_zero() {
                return Ok(());
            }

            let derivative_index = T::DerivativeIndex::get();
            let ledger: StakingLedger<T::AccountId, BalanceOf<T>> =
                Self::staking_ledgers(derivative_index).ok_or(Error::<T>::NotBonded)?;
            ensure!(
                ledger.unlocking.len() < MAX_UNLOCKING_CHUNKS,
                Error::<T>::NoMoreChunks
            );

            log::trace!(
                target: "liquidStaking::unbond",
                "amount: {:?}",
                &amount,
            );

            let query_id = T::XCM::do_unbond(
                amount,
                Self::staking_currency()?,
                derivative_index,
                Self::notify_placeholder(),
            )?;

            XcmRequests::<T>::insert(query_id, XcmRequest::Unbond { amount });

            Self::deposit_event(Event::<T>::Unbonding(amount));

            Ok(())
        }

        #[require_transactional]
        fn do_rebond(amount: BalanceOf<T>) -> DispatchResult {
            if amount.is_zero() {
                return Ok(());
            }

            log::trace!(
                target: "liquidStaking::rebond",
                "amount: {:?}",
                &amount,
            );

            let query_id = T::XCM::do_rebond(
                amount,
                Self::staking_currency()?,
                T::DerivativeIndex::get(),
                Self::notify_placeholder(),
            )?;

            XcmRequests::<T>::insert(query_id, XcmRequest::Rebond { amount });

            Self::deposit_event(Event::<T>::Rebonding(amount));

            Ok(())
        }

        #[require_transactional]
        fn do_withdraw_unbonded(num_slashing_spans: u32) -> DispatchResult {
            let query_id = T::XCM::do_withdraw_unbonded(
                num_slashing_spans,
                Self::para_account_id(),
                Self::staking_currency()?,
                T::DerivativeIndex::get(),
                Self::notify_placeholder(),
            )?;

            log::trace!(
                target: "liquidStaking::withdraw_unbonded",
                "num_slashing_spans: {:?}",
                &num_slashing_spans,
            );

            XcmRequests::<T>::insert(
                query_id,
                XcmRequest::WithdrawUnbonded { num_slashing_spans },
            );
            Self::deposit_event(Event::<T>::WithdrawingUnbonded(num_slashing_spans));
            Ok(())
        }

        #[require_transactional]
        fn do_notification_received(
            query_id: QueryId,
            request: XcmRequest<T>,
            res: Option<(u32, XcmError)>,
        ) -> DispatchResult {
            use ArithmeticKind::*;
            use XcmRequest::*;

            let executed = res.is_none();
            if !executed {
                return Ok(());
            }

            let derivative_index = T::DerivativeIndex::get();
            match request {
                Bond { amount } => {
                    ensure!(
                        !StakingLedgers::<T>::contains_key(&derivative_index),
                        Error::<T>::AlreadyBonded
                    );
                    let staking_ledger = <StakingLedger<T::AccountId, BalanceOf<T>>>::new(
                        Self::derivative_para_account_id(),
                        amount,
                    );
                    StakingLedgers::<T>::insert(derivative_index, staking_ledger);
                    MatchingPool::<T>::try_mutate(|p| -> DispatchResult {
                        p.update_total_stake_amount(amount, Subtraction)
                    })?;
                    T::Assets::burn_from(Self::staking_currency()?, &Self::account_id(), amount)?;
                }
                BondExtra { amount } => {
                    Self::do_update_ledger(derivative_index, |ledger| {
                        ledger.bond_extra(amount);
                        Ok(())
                    })?;
                    MatchingPool::<T>::try_mutate(|p| -> DispatchResult {
                        p.update_total_stake_amount(amount, Subtraction)
                    })?;
                    T::Assets::burn_from(Self::staking_currency()?, &Self::account_id(), amount)?;
                }
                Unbond { amount } => {
                    let target_era = Self::current_era() + T::BondingDuration::get();
                    Self::do_update_ledger(derivative_index, |ledger| {
                        ledger.unbond(amount, target_era);
                        Ok(())
                    })?;
                    MatchingPool::<T>::try_mutate(|p| -> DispatchResult {
                        p.update_total_unstake_amount(amount, Subtraction)
                    })?;
                }
                Rebond { amount } => {
                    Self::do_update_ledger(derivative_index, |ledger| {
                        ledger.rebond(amount);
                        Ok(())
                    })?;
                    MatchingPool::<T>::try_mutate(|p| -> DispatchResult {
                        p.update_total_stake_amount(amount, Subtraction)
                    })?;
                }
                WithdrawUnbonded {
                    num_slashing_spans: _,
                } => {
                    // TODO: we may dont have staking ledger yet
                    Self::do_update_ledger(derivative_index, |ledger| {
                        let total = ledger.total;
                        ledger.consolidate_unlocked(Self::current_era());
                        let amount = total.saturating_sub(ledger.total);
                        T::Assets::mint_into(
                            Self::staking_currency()?,
                            &Self::account_id(),
                            amount,
                        )?;
                        Ok(())
                    })?;
                }
                Nominate { targets: _ } => {}
            }
            XcmRequests::<T>::remove(&query_id);
            Ok(())
        }

        #[require_transactional]
        fn do_update_exchange_rate(bonding_amount: BalanceOf<T>) -> DispatchResult {
            let matching_ledger = Self::matching_pool();
            let issuance = T::Assets::total_issuance(Self::liquid_currency()?);
            if issuance.is_zero() {
                return Ok(());
            }
            let new_exchange_rate = Rate::checked_from_rational(
                bonding_amount
                    .checked_add(matching_ledger.total_stake_amount)
                    .and_then(|r| r.checked_sub(matching_ledger.total_unstake_amount))
                    .ok_or(ArithmeticError::Overflow)?,
                issuance,
            )
            .ok_or(Error::<T>::InvalidExchangeRate)?;
            if new_exchange_rate != Self::exchange_rate() {
                ExchangeRate::<T>::put(new_exchange_rate);
                Self::deposit_event(Event::<T>::ExchangeRateUpdated(new_exchange_rate));
            }
            Ok(())
        }

        #[require_transactional]
        fn do_update_ledger(
            derivative_index: DerivativeIndex,
            cb: impl FnOnce(&mut StakingLedger<T::AccountId, BalanceOf<T>>) -> DispatchResult,
        ) -> DispatchResult {
            StakingLedgers::<T>::try_mutate(derivative_index, |ledger| -> DispatchResult {
                let ledger = ledger.as_mut().ok_or(Error::<T>::NotBonded)?;
                cb(ledger)?;
                Ok(())
            })
        }

        #[require_transactional]
        pub(crate) fn do_advance_era(offset: EraIndex) -> DispatchResult {
            if offset.is_zero() {
                return Ok(());
            }
            EraStartBlock::<T>::put(T::RelayChainBlockNumberProvider::current_block_number());
            CurrentEra::<T>::mutate(|e| *e = e.saturating_add(offset));

            let derivative_index = T::DerivativeIndex::get();
            let ledger = StakingLedgers::<T>::get(&derivative_index);
            let unbonding_amount = ledger.map_or(Zero::zero(), |ledger| {
                ledger.total.saturating_sub(ledger.active)
            });

            // TODO: add num_slashing_spans config
            if !unbonding_amount.is_zero() {
                Self::do_withdraw_unbonded(0)?;
            }

            let (bond_amount, rebond_amount, unbond_amount) =
                Self::matching_pool().matching(unbonding_amount)?;
            if !StakingLedgers::<T>::contains_key(&derivative_index) {
                Self::do_bond(bond_amount, RewardDestination::Staked)?;
            } else {
                Self::do_bond_extra(bond_amount)?;
            }

            Self::do_unbond(unbond_amount)?;
            Self::do_rebond(rebond_amount)?;

            log::trace!(
                target: "liquidStaking::do_advance_era",
                "offset: {:?}, bond_amount: {:?}, rebond_amount: {:?}, unbond_amount: {:?}",
                &offset,
                &bond_amount,
                &rebond_amount,
                &unbond_amount,
            );

            Ok(())
        }

        fn ensure_origin(origin: OriginFor<T>) -> DispatchResult {
            if T::RelayOrigin::ensure_origin(origin.clone()).is_ok() {
                return Ok(());
            }
            let who = ensure_signed(origin)?;
            if !T::Members::contains(&who) {
                return Err(BadOrigin.into());
            }
            Ok(())
        }

        fn ensure_market_cap(asset_id: AssetIdOf<T>, amount: BalanceOf<T>) -> DispatchResult {
            let issuance = T::Assets::total_issuance(asset_id);
            let new_issurance = issuance
                .checked_add(amount)
                .ok_or(ArithmeticError::Overflow)?;
            ensure!(new_issurance <= Self::market_cap(), Error::<T>::CapExceeded);
            Ok(())
        }

        fn notify_placeholder() -> <T as Config>::Call {
            <T as Config>::Call::from(Call::<T>::notification_received {
                query_id: Default::default(),
                response: Default::default(),
            })
        }
    }
}

impl<T: Config> ExchangeRateProvider for Pallet<T> {
    fn get_exchange_rate() -> Rate {
        ExchangeRate::<T>::get()
    }
}

impl<T: Config> LiquidStakingCurrenciesProvider<AssetIdOf<T>> for Pallet<T> {
    fn get_staking_currency() -> Option<AssetIdOf<T>> {
        let asset_id = T::StakingCurrency::get();
        if !<T::Assets as InspectMetadata<AccountIdOf<T>>>::decimals(&asset_id).is_zero() {
            Some(asset_id)
        } else {
            None
        }
    }

    fn get_liquid_currency() -> Option<AssetIdOf<T>> {
        let asset_id = T::LiquidCurrency::get();
        if !<T::Assets as InspectMetadata<AccountIdOf<T>>>::decimals(&asset_id).is_zero() {
            Some(asset_id)
        } else {
            None
        }
    }
}

impl<T: Config, Balance: BalanceT + FixedPointOperand> LiquidStakingConvert<Balance> for Pallet<T> {
    fn staking_to_liquid(amount: Balance) -> Option<Balance> {
        Self::exchange_rate()
            .reciprocal()
            .and_then(|r| r.checked_mul_int(amount))
    }

    fn liquid_to_staking(liquid_amount: Balance) -> Option<Balance> {
        Self::exchange_rate().checked_mul_int(liquid_amount)
    }
}
