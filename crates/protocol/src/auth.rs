//! 加入房間的挑戰／證明。
//!
//! 房主送隨機 nonce，客戶端回 `blake3_keyed(room_secret, "multivoice-join-v1" ‖ room_id ‖ nonce ‖ client_node_id)`。
//! 綁定 client_node_id（QUIC/TLS 已驗證過的身分），讓攔截到的證明無法被另一個身分重用；
//! 每次連線的 nonce 不同，所以也無法重播。

use crate::NodeId;
use crate::ticket::{RoomId, RoomSecret};

const DOMAIN: &[u8] = b"multivoice-join-v1";

pub type Nonce = [u8; 32];
pub type Proof = [u8; 32];

pub fn join_proof(secret: &RoomSecret, room: &RoomId, nonce: &Nonce, client: &NodeId) -> Proof {
    let mut h = blake3::Hasher::new_keyed(&secret.0);
    h.update(DOMAIN);
    h.update(&room.0);
    h.update(nonce);
    h.update(client);
    *h.finalize().as_bytes()
}

/// 常數時間比較（blake3::Hash 的 `==` 即為常數時間）
pub fn verify_join_proof(
    secret: &RoomSecret,
    room: &RoomId,
    nonce: &Nonce,
    client: &NodeId,
    proof: &Proof,
) -> bool {
    blake3::Hash::from_bytes(join_proof(secret, room, nonce, client)) == blake3::Hash::from_bytes(*proof)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proof_binds_everything() {
        let (s, r, n, c) = (RoomSecret([1; 32]), RoomId([2; 32]), [3; 32], [4; 32]);
        let p = join_proof(&s, &r, &n, &c);
        assert!(verify_join_proof(&s, &r, &n, &c, &p));
        assert!(!verify_join_proof(&RoomSecret([9; 32]), &r, &n, &c, &p), "錯誤的密鑰");
        assert!(!verify_join_proof(&s, &RoomId([9; 32]), &n, &c, &p), "別的房間");
        assert!(!verify_join_proof(&s, &r, &[9; 32], &c, &p), "重播（nonce 不同）");
        assert!(!verify_join_proof(&s, &r, &n, &[9; 32], &p), "換身分重用");
    }
}
