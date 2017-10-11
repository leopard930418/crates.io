use std::collections::HashMap;

use chrono::NaiveDateTime;
use conduit::{Request, Response};
use conduit_router::RequestParams;
use diesel;
use diesel::pg::Pg;
use diesel::prelude::*;
use semver;
use serde_json;

use Crate;
use db::RequestTransaction;
use dependency::{Dependency, EncodableDependency};
use schema::*;
use util::{human, CargoResult, RequestUtils};
use license_exprs;

pub mod deprecated;
pub mod downloads;
pub mod yank;

// Queryable has a custom implementation below
#[derive(Clone, Identifiable, Associations, Debug)]
#[belongs_to(Crate)]
pub struct Version {
    pub id: i32,
    pub crate_id: i32,
    pub num: semver::Version,
    pub updated_at: NaiveDateTime,
    pub created_at: NaiveDateTime,
    pub downloads: i32,
    pub features: HashMap<String, Vec<String>>,
    pub yanked: bool,
    pub license: Option<String>,
}

#[derive(Insertable, Debug)]
#[table_name = "versions"]
pub struct NewVersion {
    crate_id: i32,
    num: String,
    features: String,
    license: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct EncodableVersion {
    pub id: i32,
    #[serde(rename = "crate")] pub krate: String,
    pub num: String,
    pub dl_path: String,
    pub readme_path: String,
    pub updated_at: NaiveDateTime,
    pub created_at: NaiveDateTime,
    pub downloads: i32,
    pub features: HashMap<String, Vec<String>>,
    pub yanked: bool,
    pub license: Option<String>,
    pub links: VersionLinks,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct VersionLinks {
    pub dependencies: String,
    pub version_downloads: String,
    pub authors: String,
}

impl Version {
    pub fn encodable(self, crate_name: &str) -> EncodableVersion {
        let Version {
            id,
            num,
            updated_at,
            created_at,
            downloads,
            features,
            yanked,
            license,
            ..
        } = self;
        let num = num.to_string();
        EncodableVersion {
            dl_path: format!("/api/v1/crates/{}/{}/download", crate_name, num),
            readme_path: format!("/api/v1/crates/{}/{}/readme", crate_name, num),
            num: num.clone(),
            id: id,
            krate: crate_name.to_string(),
            updated_at: updated_at,
            created_at: created_at,
            downloads: downloads,
            features: features,
            yanked: yanked,
            license: license,
            links: VersionLinks {
                dependencies: format!("/api/v1/crates/{}/{}/dependencies", crate_name, num),
                version_downloads: format!("/api/v1/crates/{}/{}/downloads", crate_name, num),
                authors: format!("/api/v1/crates/{}/{}/authors", crate_name, num),
            },
        }
    }

    /// Returns (dependency, crate dependency name)
    pub fn dependencies(&self, conn: &PgConnection) -> QueryResult<Vec<(Dependency, String)>> {
        Dependency::belonging_to(self)
            .inner_join(crates::table)
            .select((dependencies::all_columns, crates::name))
            .order((dependencies::optional, crates::name))
            .load(conn)
    }

    pub fn max<T>(versions: T) -> semver::Version
    where
        T: IntoIterator<Item = semver::Version>,
    {
        versions.into_iter().max().unwrap_or_else(|| {
            semver::Version {
                major: 0,
                minor: 0,
                patch: 0,
                pre: vec![],
                build: vec![],
            }
        })
    }

    pub fn record_readme_rendering(&self, conn: &PgConnection) -> QueryResult<usize> {
        use schema::versions::dsl::readme_rendered_at;
        use diesel::expression::now;

        diesel::update(self)
            .set(readme_rendered_at.eq(now.nullable()))
            .execute(conn)
    }
}

impl NewVersion {
    pub fn new(
        crate_id: i32,
        num: &semver::Version,
        features: &HashMap<String, Vec<String>>,
        license: Option<String>,
        license_file: Option<&str>,
    ) -> CargoResult<Self> {
        let features = serde_json::to_string(features)?;

        let mut new_version = NewVersion {
            crate_id: crate_id,
            num: num.to_string(),
            features: features,
            license: license,
        };

        new_version.validate_license(license_file)?;

        Ok(new_version)
    }

    pub fn save(&self, conn: &PgConnection, authors: &[String]) -> CargoResult<Version> {
        use diesel::{insert, select};
        use diesel::expression::dsl::exists;
        use schema::versions::dsl::*;

        let already_uploaded = versions
            .filter(crate_id.eq(self.crate_id))
            .filter(num.eq(&self.num));
        if select(exists(already_uploaded)).get_result(conn)? {
            return Err(human(&format_args!(
                "crate version `{}` is already \
                 uploaded",
                self.num
            )));
        }

        conn.transaction(|| {
            let version = insert(self).into(versions).get_result::<Version>(conn)?;

            let new_authors = authors
                .iter()
                .map(|s| {
                    NewAuthor {
                        version_id: version.id,
                        name: &*s,
                    }
                })
                .collect::<Vec<_>>();

            insert(&new_authors)
                .into(version_authors::table)
                .execute(conn)?;
            Ok(version)
        })
    }

    fn validate_license(&mut self, license_file: Option<&str>) -> CargoResult<()> {
        if let Some(ref license) = self.license {
            for part in license.split('/') {
                license_exprs::validate_license_expr(part).map_err(|e| {
                    human(&format_args!(
                        "{}; see http://opensource.org/licenses \
                         for options, and http://spdx.org/licenses/ \
                         for their identifiers",
                        e
                    ))
                })?;
            }
        } else if license_file.is_some() {
            // If no license is given, but a license file is given, flag this
            // crate as having a nonstandard license. Note that we don't
            // actually do anything else with license_file currently.
            self.license = Some(String::from("non-standard"));
        }
        Ok(())
    }
}

#[derive(Insertable, Debug)]
#[table_name = "version_authors"]
struct NewAuthor<'a> {
    version_id: i32,
    name: &'a str,
}

impl Queryable<versions::SqlType, Pg> for Version {
    #[cfg_attr(feature = "clippy", allow(type_complexity))]
    type Row = (
        i32,
        i32,
        String,
        NaiveDateTime,
        NaiveDateTime,
        i32,
        Option<String>,
        bool,
        Option<String>,
        Option<NaiveDateTime>,
    );

    fn build(row: Self::Row) -> Self {
        let features = row.6
            .map(|s| serde_json::from_str(&s).unwrap())
            .unwrap_or_else(HashMap::new);
        Version {
            id: row.0,
            crate_id: row.1,
            num: semver::Version::parse(&row.2).unwrap(),
            updated_at: row.3,
            created_at: row.4,
            downloads: row.5,
            features: features,
            yanked: row.7,
            license: row.8,
        }
    }
}

fn version_and_crate(req: &mut Request) -> CargoResult<(Version, Crate)> {
    let crate_name = &req.params()["crate_id"];
    let semver = &req.params()["version"];
    if semver::Version::parse(semver).is_err() {
        return Err(human(&format_args!("invalid semver: {}", semver)));
    };
    let conn = req.db_conn()?;
    let krate = Crate::by_name(crate_name).first::<Crate>(&*conn)?;
    let version = Version::belonging_to(&krate)
        .filter(versions::num.eq(semver))
        .first(&*conn)
        .map_err(|_| {
            human(&format_args!(
                "crate `{}` does not have a version `{}`",
                crate_name,
                semver
            ))
        })?;
    Ok((version, krate))
}

/// Handles the `GET /crates/:crate_id/:version/dependencies` route.
pub fn dependencies(req: &mut Request) -> CargoResult<Response> {
    let (version, _) = version_and_crate(req)?;
    let conn = req.db_conn()?;
    let deps = version.dependencies(&*conn)?;
    let deps = deps.into_iter()
        .map(|(dep, crate_name)| dep.encodable(&crate_name, None))
        .collect();

    #[derive(Serialize)]
    struct R {
        dependencies: Vec<EncodableDependency>,
    }
    Ok(req.json(&R { dependencies: deps }))
}

/// Handles the `GET /crates/:crate_id/:version/authors` route.
pub fn authors(req: &mut Request) -> CargoResult<Response> {
    let (version, _) = version_and_crate(req)?;
    let conn = req.db_conn()?;
    let names = version_authors::table
        .filter(version_authors::version_id.eq(version.id))
        .select(version_authors::name)
        .order(version_authors::name)
        .load(&*conn)?;

    // It was imagined that we wold associate authors with users.
    // This was never implemented. This complicated return struct
    // is all that is left, hear for backwards compatibility.
    #[derive(Serialize)]
    struct R {
        users: Vec<::user::EncodablePublicUser>,
        meta: Meta,
    }
    #[derive(Serialize)]
    struct Meta {
        names: Vec<String>,
    }
    Ok(req.json(&R {
        users: vec![],
        meta: Meta { names: names },
    }))
}
