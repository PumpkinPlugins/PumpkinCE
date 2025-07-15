use async_trait::async_trait;
use pumpkin_data::Block;
use pumpkin_data::block_properties::{
    BlockProperties, EnumVariants, Integer0To1, TorchflowerCropLikeProperties,
};
use pumpkin_macros::pumpkin_block;
use pumpkin_world::BlockStateId;
use rand::Rng;

use crate::block::blocks::plant::PlantBlockBase;
use crate::block::blocks::plant::crop::CropBlockBase;
use crate::block::pumpkin_block::{
    CanPlaceAtArgs, GetStateForNeighborUpdateArgs, PumpkinBlock, RandomTickArgs,
};

type TorchFlowerProperties = TorchflowerCropLikeProperties;

#[pumpkin_block("minecraft:torchflower_crop")]
pub struct TorchFlowerBlock;

#[async_trait]
impl PumpkinBlock for TorchFlowerBlock {
    async fn can_place_at(&self, args: CanPlaceAtArgs<'_>) -> bool {
        <Self as CropBlockBase>::can_plant_on_top(self, args.block_accessor, &args.position.down())
            .await
    }

    async fn get_state_for_neighbor_update(
        &self,
        args: GetStateForNeighborUpdateArgs<'_>,
    ) -> BlockStateId {
        <Self as PlantBlockBase>::get_state_for_neighbor_update(
            self,
            args.world,
            args.position,
            args.state_id,
        )
        .await
    }

    async fn random_tick(&self, args: RandomTickArgs<'_>) {
        if rand::rng().random_range(0..2) != 0 {
            <Self as CropBlockBase>::random_tick(self, args.world, args.position).await;
        }
    }
}

impl PlantBlockBase for TorchFlowerBlock {}

impl CropBlockBase for TorchFlowerBlock {
    fn max_age(&self) -> i32 {
        2
    }

    fn get_age(&self, state: &pumpkin_data::BlockState, block: &Block) -> i32 {
        let props = TorchFlowerProperties::from_state_id(state.id, block);
        i32::from(props.age.to_index())
    }

    fn state_with_age(
        &self,
        block: &Block,
        state: &pumpkin_data::BlockState,
        age: i32,
    ) -> BlockStateId {
        if age == 1 {
            let mut properties = TorchFlowerProperties::from_state_id(state.id, block);
            properties.age = Integer0To1::L1;
            properties.to_state_id(block)
        } else {
            Block::TORCHFLOWER.default_state.id
        }
    }
}
