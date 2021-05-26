//! Endpoint for exposing crate download counts
//!
//! The enpoints for download a crate and exposing version specific
//! download counts are located in `krate::downloads`.

use std::cmp;

use crate::controllers::frontend_prelude::*;

use crate::models::{Crate, CrateVersions, Version, VersionDownload};
use crate::schema::version_downloads;
use crate::views::EncodableVersionDownload;

use crate::models::krate::to_char;

/// Handles the `GET /crates/:crate_id/downloads` route.
pub fn downloads(req: &mut dyn RequestExt) -> EndpointResult {
    use diesel::dsl::*;
    use diesel::sql_types::BigInt;

    let crate_name = &req.params()["crate_id"];
    let conn = req.db_read_only()?;
    let krate: Crate = Crate::by_name(crate_name).first(&*conn)?;

    let mut versions: Vec<Version> = krate.all_versions().load(&*conn)?;
    versions.sort_by_cached_key(|version| cmp::Reverse(semver::Version::parse(&version.num).ok()));
    let (latest_five, rest) = versions.split_at(cmp::min(5, versions.len()));

    let downloads = VersionDownload::belonging_to(latest_five)
        .filter(version_downloads::date.gt(date(now - 90.days())))
        .order(version_downloads::date.asc())
        .load(&*conn)?
        .into_iter()
        .map(VersionDownload::into)
        .collect::<Vec<_>>();

    let sum_downloads = sql::<BigInt>("SUM(version_downloads.downloads)");
    let extra: Vec<ExtraDownload> = VersionDownload::belonging_to(rest)
        .select((
            to_char(version_downloads::date, "YYYY-MM-DD"),
            sum_downloads,
        ))
        .filter(version_downloads::date.gt(date(now - 90.days())))
        .group_by(version_downloads::date)
        .order(version_downloads::date.asc())
        .load(&*conn)?;

    #[derive(Serialize, Queryable)]
    struct ExtraDownload {
        date: String,
        downloads: i64,
    }
    #[derive(Serialize)]
    struct R {
        version_downloads: Vec<EncodableVersionDownload>,
        meta: Meta,
    }
    #[derive(Serialize)]
    struct Meta {
        extra_downloads: Vec<ExtraDownload>,
    }
    let meta = Meta {
        extra_downloads: extra,
    };
    Ok(req.json(&R {
        version_downloads: downloads,
        meta,
    }))
}
