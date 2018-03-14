use conduit::Response;
use util::{json_response, CargoResult};

pub mod pagination;

pub use self::pagination::Paginate;

pub fn ok_true() -> CargoResult<Response> {
    #[derive(Serialize)]
    struct R {
        ok: bool,
    }

    Ok(json_response(&R { ok: true }))
}
