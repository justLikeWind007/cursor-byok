use bytes::Bytes;
use cursor_server::{cursor::connect, Result};
use prost::Message;

pub fn decode_single<M: Message + Default>(frame: &Bytes) -> Result<M> {
    let frames = connect::decode_frames(frame)?;
    assert_eq!(frames.len(), 1);
    Ok(M::decode(frames[0].1.clone())?)
}
