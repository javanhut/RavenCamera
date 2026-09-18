//! Recording: capturing to disk while it happens ([`live`]), what a
//! recording on disk is ([`session`], [`camfile`]), and making the finished
//! file from it ([`export`]).

pub mod camfile;
pub mod export;
pub mod live;
pub mod session;
