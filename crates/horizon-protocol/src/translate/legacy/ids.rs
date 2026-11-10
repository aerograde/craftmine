//! Protocol 47's packet ids for the packets a shared world's guest needs.
//! 1.8 has no configuration state: after the login the play state follows.

pub mod login {
    pub mod serverbound {
        pub const LOGIN_START: i32 = 0x00;
    }
    pub mod clientbound {
        pub const DISCONNECT: i32 = 0x00;
        pub const ENCRYPTION_REQUEST: i32 = 0x01;
        pub const LOGIN_SUCCESS: i32 = 0x02;
        pub const SET_COMPRESSION: i32 = 0x03;
    }
}

pub mod play {
    pub mod clientbound {
        pub const KEEP_ALIVE: i32 = 0x00;
        pub const JOIN_GAME: i32 = 0x01;
        pub const CHAT: i32 = 0x02;
        pub const TIME_UPDATE: i32 = 0x03;
        pub const ENTITY_EQUIPMENT: i32 = 0x04;
        pub const SPAWN_POSITION: i32 = 0x05;
        pub const UPDATE_HEALTH: i32 = 0x06;
        pub const RESPAWN: i32 = 0x07;
        pub const PLAYER_POSITION_AND_LOOK: i32 = 0x08;
        pub const HELD_ITEM_CHANGE: i32 = 0x09;
        pub const ANIMATION: i32 = 0x0B;
        pub const SPAWN_OBJECT: i32 = 0x0E;
        pub const SPAWN_MOB: i32 = 0x0F;
        pub const ENTITY_VELOCITY: i32 = 0x12;
        pub const DESTROY_ENTITIES: i32 = 0x13;
        pub const ENTITY_REL_MOVE: i32 = 0x15;
        pub const ENTITY_LOOK: i32 = 0x16;
        pub const ENTITY_LOOK_AND_REL_MOVE: i32 = 0x17;
        pub const ENTITY_TELEPORT: i32 = 0x18;
        pub const ENTITY_HEAD_LOOK: i32 = 0x19;
        pub const CHUNK_DATA: i32 = 0x21;
        pub const MULTI_BLOCK_CHANGE: i32 = 0x22;
        pub const BLOCK_CHANGE: i32 = 0x23;
        pub const MAP_CHUNK_BULK: i32 = 0x26;
        pub const CLOSE_WINDOW: i32 = 0x2E;
        pub const SET_SLOT: i32 = 0x2F;
        pub const WINDOW_ITEMS: i32 = 0x30;
        pub const WINDOW_PROPERTY: i32 = 0x31;
        pub const CHANGE_GAME_STATE: i32 = 0x2B;
        pub const PLAYER_LIST_ITEM: i32 = 0x38;
        pub const PLAYER_ABILITIES: i32 = 0x39;
    }
    pub mod serverbound {
        pub const KEEP_ALIVE: i32 = 0x00;
        pub const CHAT: i32 = 0x01;
        pub const PLAYER: i32 = 0x03;
        pub const PLAYER_POSITION: i32 = 0x04;
        pub const PLAYER_LOOK: i32 = 0x05;
        pub const PLAYER_POSITION_AND_LOOK: i32 = 0x06;
        pub const PLAYER_DIGGING: i32 = 0x07;
        pub const PLAYER_BLOCK_PLACEMENT: i32 = 0x08;
        pub const HELD_ITEM_CHANGE: i32 = 0x09;
        pub const ANIMATION: i32 = 0x0A;
        pub const ENTITY_ACTION: i32 = 0x0B;
        pub const CLIENT_SETTINGS: i32 = 0x15;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A few ids every 1.8 client knows; a table that drifts fails here.
    #[test]
    fn the_well_known_ids() {
        assert_eq!(login::clientbound::LOGIN_SUCCESS, 0x02);
        assert_eq!(play::clientbound::KEEP_ALIVE, 0x00);
        assert_eq!(play::clientbound::JOIN_GAME, 0x01);
        assert_eq!(play::clientbound::CHUNK_DATA, 0x21);
        assert_eq!(play::clientbound::BLOCK_CHANGE, 0x23);
        assert_eq!(play::serverbound::PLAYER_POSITION_AND_LOOK, 0x06);
    }
}
