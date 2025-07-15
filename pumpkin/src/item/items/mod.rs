mod axe;
mod bucket;
mod egg;
mod ender_eye;
mod hoe;
mod honeycomb;
mod ignite;
mod mace;
mod minecart;
mod shovel;
mod snowball;
mod swords;
mod trident;

use crate::item::items::minecart::MinecartItem;

use super::registry::ItemRegistry;
use axe::AxeItem;
use bucket::{EmptyBucketItem, FilledBucketItem};
use egg::EggItem;
use ender_eye::EnderEyeItem;
use hoe::HoeItem;
use honeycomb::HoneyCombItem;
use ignite::fire_charge::FireChargeItem;
use ignite::flint_and_steel::FlintAndSteelItem;
use mace::MaceItem;
use shovel::ShovelItem;
use snowball::SnowBallItem;
use std::sync::Arc;
use swords::SwordItem;
use trident::TridentItem;

#[must_use]
pub fn default_registry() -> Arc<ItemRegistry> {
    let mut manager = ItemRegistry::default();

    manager.register(SnowBallItem);
    manager.register(HoeItem);
    manager.register(EggItem);
    manager.register(FlintAndSteelItem);
    manager.register(SwordItem);
    manager.register(MaceItem);
    manager.register(TridentItem);
    manager.register(EmptyBucketItem);
    manager.register(FilledBucketItem);
    manager.register(ShovelItem);
    manager.register(AxeItem);
    manager.register(MinecartItem);
    manager.register(HoneyCombItem);
    manager.register(EnderEyeItem);
    manager.register(FireChargeItem);

    Arc::new(manager)
}
