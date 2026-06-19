//! winkb — a read-only knowledge layer over `windows_api.db` (winmd-derived).
//!
//! One thin rusqlite wrapper, queried two ways: by the IDE pane (search,
//! show-with-related) and by the assembler front-end (resolve a name to a value,
//! a struct field to an offset, a function to its DLL + signature). The database
//! is the single source of truth — nothing is generated or cached to disk.
//!
//! Opened read-only; the file is never modified.

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension};

/// The knowledge base: a read-only connection to `windows_api.db`.
pub struct Kb {
    conn: Connection,
}

/// A search result row.
#[derive(Debug, Clone)]
pub struct Hit {
    pub name: String,
    /// `function` | `constant` | `enum-val` | `struct` | `union` | `enum` | …
    pub kind: String,
    /// DLL for functions, value-kind for constants, qualified name for types, …
    pub detail: String,
}

/// A named integer value resolved from `constants` or `enum_members`.
#[derive(Debug, Clone)]
pub struct Value {
    /// `const` or `enum`.
    pub source: String,
    /// Sign-interpreted value.
    pub i64v: i64,
    /// Raw bit pattern at the declared width (what the assembler emits).
    pub bits: u64,
    pub namespace: Option<String>,
}

/// One parameter of a function, with the constants that belong to its type.
#[derive(Debug, Clone)]
pub struct Param {
    pub ordinal: i64,
    pub name: String,
    pub type_name: String,
    pub type_kind: String,
    /// For an enum-typed param: the `(member, value)` constants it accepts.
    pub related: Vec<(String, u64)>,
}

/// A function: signature plus the related constants per parameter.
#[derive(Debug, Clone)]
pub struct Func {
    pub name: String,
    pub dll: Option<String>,
    pub callconv: Option<String>,
    pub aw_family: Option<String>,
    pub ret: String,
    pub doc_url: Option<String>,
    pub params: Vec<Param>,
}

impl Kb {
    /// Open `windows_api.db` read-only.
    pub fn open(path: &str) -> Result<Kb> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("open {path} read-only"))?;
        Ok(Kb { conn })
    }

    /// ① Search functions / constants / enum members / types by name substring.
    /// Exact and prefix matches sort first, then shortest names.
    pub fn search(&self, frag: &str, limit: usize) -> Result<Vec<Hit>> {
        let substr = format!("%{frag}%");
        let prefix = format!("{frag}%");
        let mut stmt = self.conn.prepare(
            "SELECT name, kind, detail FROM (
               SELECT function_name AS name, 'function' AS kind, COALESCE(dll_name,'') AS detail
                 FROM functions WHERE function_name LIKE ?1
               UNION ALL SELECT constant_name, 'constant', value_kind
                 FROM constants WHERE constant_name LIKE ?1
               UNION ALL SELECT member_name, 'enum-val', COALESCE(underlying_type,'')
                 FROM enum_members WHERE member_name LIKE ?1
               UNION ALL SELECT type_name, kind, COALESCE(qualified_name,'')
                 FROM types WHERE type_name LIKE ?1
             )
             ORDER BY (name = ?2) DESC, (name LIKE ?3) DESC, length(name), name
             LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![substr, frag, prefix, limit as i64],
            |r| {
                Ok(Hit {
                    name: r.get(0)?,
                    kind: r.get(1)?,
                    detail: r.get(2)?,
                })
            },
        )?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// ③ Resolve a bare name to its value(s), unifying constants and enum members.
    /// Returns every match (usually one); the caller disambiguates on collision.
    pub fn resolve(&self, name: &str) -> Result<Vec<Value>> {
        let mut stmt = self.conn.prepare(
            "SELECT 'const' AS src, value_i64, value_u64, namespace_name
               FROM constants WHERE constant_name = ?1
             UNION ALL
             SELECT 'enum', em.value_i64, em.value_u64, t.namespace_name
               FROM enum_members em JOIN types t ON t.type_id = em.enum_type_id
               WHERE em.member_name = ?1",
        )?;
        let rows = stmt.query_map([name], |r| {
            let i64v: Option<i64> = r.get(1)?;
            let bits: Option<i64> = r.get(2)?;
            Ok(Value {
                source: r.get(0)?,
                i64v: i64v.unwrap_or(0),
                bits: bits.unwrap_or(0) as u64,
                namespace: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// ② A function's signature plus, per enum-typed parameter, the constants it
    /// accepts (the "related constants/types" the pane shows). Takes the first
    /// match if the name occurs in more than one namespace.
    pub fn function(&self, name: &str) -> Result<Option<Func>> {
        let head = self
            .conn
            .query_row(
                "SELECT f.function_id, f.dll_name, f.callconv, f.aw_family,
                        f.documentation_url, t.type_name
                   FROM functions f LEFT JOIN types t ON t.type_id = f.return_type_id
                   WHERE f.function_name = ?1
                   LIMIT 1",
                [name],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, Option<String>>(5)?,
                    ))
                },
            )
            .optional()?;

        let Some((fid, dll, callconv, aw_family, doc_url, ret)) = head else {
            return Ok(None);
        };

        let mut pstmt = self.conn.prepare(
            "SELECT p.ordinal, p.param_name, t.type_name, t.kind, t.type_id
               FROM function_params p LEFT JOIN types t ON t.type_id = p.type_id
               WHERE p.function_id = ?1 ORDER BY p.ordinal",
        )?;
        let raw: Vec<(i64, Option<String>, Option<String>, Option<String>, Option<i64>)> = pstmt
            .query_map([fid], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<_>>()?;

        let mut params = Vec::with_capacity(raw.len());
        for (ordinal, pname, tname, kind, tid) in raw {
            let type_kind = kind.unwrap_or_default();
            let related = if type_kind == "enum" {
                if let Some(tid) = tid {
                    self.enum_members(tid)?
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            };
            params.push(Param {
                ordinal,
                name: pname.unwrap_or_default(),
                type_name: tname.unwrap_or_else(|| "?".into()),
                type_kind,
                related,
            });
        }

        Ok(Some(Func {
            name: name.to_string(),
            dll,
            callconv,
            aw_family,
            ret: ret.unwrap_or_else(|| "void".into()),
            doc_url,
            params,
        }))
    }

    /// The `(member, value)` constants of an enum type.
    pub fn enum_members(&self, enum_type_id: i64) -> Result<Vec<(String, u64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT member_name, value_u64 FROM enum_members
               WHERE enum_type_id = ?1 ORDER BY ordinal",
        )?;
        let rows = stmt.query_map([enum_type_id], |r| {
            let v: Option<i64> = r.get(1)?;
            Ok((r.get::<_, String>(0)?, v.unwrap_or(0) as u64))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
