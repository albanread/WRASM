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
use std::cell::RefCell;
use std::collections::HashMap;

/// The knowledge base: a read-only connection to `windows_api.db`.
pub struct Kb {
    conn: Connection,
    /// Memoizes [`resolve`](Kb::resolve) — the assembler calls it for *every*
    /// identifier token it lowers (mnemonics included), and a miss scans the
    /// unindexed `enum_members.member_name` (~68k rows, ~4 ms). Most tokens repeat
    /// (`mov`, a constant used many times), so one cache turns a per-line DB scan
    /// into a hash hit — the difference between seconds and milliseconds on a big
    /// single-file program. Single-threaded use only (the DB is opened NO_MUTEX).
    resolve_cache: RefCell<HashMap<String, Vec<Value>>>,
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

/// An autocomplete candidate (a prefix match).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    pub name: String,
    /// `function` | `constant` | `enum-val` | a type kind | `field`.
    pub kind: String,
    /// DLL / value-kind / qualified name / field type, by kind.
    pub detail: String,
}

/// An x86-64 instruction explainer entry (from the `instructions` table).
#[derive(Debug, Clone)]
pub struct Instruction {
    pub mnemonic: String,
    /// `data` | `arithmetic` | `logic` | `shift` | `bit` | `control-flow` |
    /// `stack` | `string` | `system` | `sse` | …
    pub category: String,
    /// Flags the instruction affects, e.g. `ZF SF CF OF` — or empty / `none`.
    pub flags: String,
    pub summary: String,
    pub description: String,
}

/// A WRASM dialect directive/macro explainer (the `directives` table).
#[derive(Debug, Clone)]
pub struct Directive {
    pub name: String,
    pub category: String,
    pub syntax: String,
    pub summary: String,
    pub description: String,
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
        Ok(Kb { conn, resolve_cache: RefCell::new(HashMap::new()) })
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

    /// Prefix-match candidates for autocomplete. `scope` selects the tables:
    /// `"function"`, `"constant"` (constants + enum members), `"type"`, or
    /// `"all"`. Ordered shortest-name-first (the prefix-completion feel).
    pub fn complete(&self, prefix: &str, scope: &str, limit: usize) -> Result<Vec<Completion>> {
        // Windows names are full of `_`, which is a LIKE wildcard — escape it (and
        // `%`/`\`) so this is a *literal* prefix match, not a fuzzy one.
        let like = format!("{}%", escape_like(prefix));
        let mut parts: Vec<&str> = Vec::new();
        if scope == "function" || scope == "all" {
            parts.push(
                "SELECT function_name AS name, 'function' AS kind, COALESCE(dll_name,'') AS detail \
                   FROM functions WHERE function_name LIKE ?1 ESCAPE '\\'",
            );
        }
        if scope == "constant" || scope == "all" {
            parts.push(
                "SELECT constant_name AS name, 'constant' AS kind, value_kind AS detail \
                   FROM constants WHERE constant_name LIKE ?1 ESCAPE '\\'",
            );
            parts.push(
                "SELECT member_name AS name, 'enum-val' AS kind, COALESCE(underlying_type,'') AS detail \
                   FROM enum_members WHERE member_name LIKE ?1 ESCAPE '\\'",
            );
        }
        if scope == "type" || scope == "all" {
            parts.push(
                "SELECT type_name AS name, kind AS kind, COALESCE(qualified_name,'') AS detail \
                   FROM types WHERE type_name LIKE ?1 ESCAPE '\\'",
            );
        }
        if parts.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT name, kind, detail FROM ({}) ORDER BY length(name), name LIMIT ?2",
            parts.join(" UNION ALL "),
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params![like, limit as i64], |r| {
            Ok(Completion { name: r.get(0)?, kind: r.get(1)?, detail: r.get(2)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The instruction explainer for `mnemonic`, or `None` if it isn't in the
    /// `instructions` table (or the table is absent — an older db). Case- and
    /// whitespace-insensitive.
    pub fn instruction(&self, mnemonic: &str) -> Result<Option<Instruction>> {
        // Tolerate a db built before the instructions table existed.
        let has_table: bool = self
            .conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='instructions'",
                [],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !has_table {
            return Ok(None);
        }
        let m = mnemonic.trim().to_ascii_lowercase();
        self.conn
            .query_row(
                "SELECT mnemonic, category, flags, summary, description \
                   FROM instructions WHERE mnemonic = ?1",
                [m],
                |r| {
                    Ok(Instruction {
                        mnemonic: r.get(0)?,
                        category: r.get(1)?,
                        flags: r.get(2)?,
                        summary: r.get(3)?,
                        description: r.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// The WRASM directive/macro explainer for `name` (the `directives` table),
    /// or `None` if absent. Case- and whitespace-insensitive.
    pub fn directive(&self, name: &str) -> Result<Option<Directive>> {
        let has: bool = self
            .conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='directives'",
                [],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !has {
            return Ok(None);
        }
        let n = name.trim().to_ascii_lowercase();
        self.conn
            .query_row(
                "SELECT name, category, syntax, summary, description \
                   FROM directives WHERE name = ?1",
                [n],
                |r| {
                    Ok(Directive {
                        name: r.get(0)?,
                        category: r.get(1)?,
                        syntax: r.get(2)?,
                        summary: r.get(3)?,
                        description: r.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// ③ Resolve a bare name to its value(s), unifying constants and enum members.
    /// Returns every match (usually one); the caller disambiguates on collision.
    pub fn resolve(&self, name: &str) -> Result<Vec<Value>> {
        if let Some(hit) = self.resolve_cache.borrow().get(name) {
            return Ok(hit.clone());
        }
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
        let values = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        // Cache hits *and* misses (the empty vec) — a mnemonic like `mov` misses
        // on every line, and not re-scanning for it is the whole point.
        self.resolve_cache.borrow_mut().insert(name.to_string(), values.clone());
        Ok(values)
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

    /// ④ Struct/union layout: `sizeof`, alignment, and each field's byte offset
    /// (all precomputed by the importer's `compute-layout` pass).
    pub fn layout(&self, name: &str) -> Result<Option<Layout>> {
        let head = self
            .conn
            .query_row(
                "SELECT type_id, kind, size_bits, align_bits FROM types
                   WHERE type_name = ?1 AND kind IN ('struct', 'union') LIMIT 1",
                [name],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                        r.get::<_, Option<i64>>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((tid, kind, size_bits, align_bits)) = head else {
            return Ok(None);
        };
        let mut stmt = self.conn.prepare(
            "SELECT sf.field_name, sf.byte_offset, t.type_name
               FROM struct_fields sf LEFT JOIN types t ON t.type_id = sf.type_id
               WHERE sf.struct_type_id = ?1 ORDER BY sf.ordinal",
        )?;
        let fields = stmt
            .query_map([tid], |r| {
                Ok(Field {
                    name: r.get(0)?,
                    offset: r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    type_name: r.get::<_, Option<String>>(2)?.unwrap_or_else(|| "?".into()),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Some(Layout {
            name: name.to_string(),
            kind,
            size: size_bits.unwrap_or(0) / 8,
            align: align_bits.unwrap_or(0) / 8,
            fields,
        }))
    }

    /// Byte size of any named type (struct/enum/typedef/…), or None if unknown.
    pub fn sizeof(&self, name: &str) -> Result<Option<i64>> {
        let bits = self
            .conn
            .query_row(
                "SELECT size_bits FROM types WHERE type_name = ?1 AND size_bits IS NOT NULL LIMIT 1",
                [name],
                |r| r.get::<_, i64>(0),
            )
            .optional()?;
        Ok(bits.map(|b| b / 8))
    }

    /// ⑥ COM interface: IID and its own methods in absolute vtable order
    /// (`call [vtable + vtable_index*8]`). Inherited slots come from `base`.
    pub fn interface(&self, name: &str) -> Result<Option<Interface>> {
        let head = self
            .conn
            .query_row(
                "SELECT type_id, iid, base_qualified_name FROM types
                   WHERE type_name = ?1 AND kind = 'interface' LIMIT 1",
                [name],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((tid, iid, base)) = head else {
            return Ok(None);
        };
        // Parameter types for every method on this interface, in one query,
        // grouped by method and ordered by ordinal (the type at ordinal N is
        // param N's type — that's all the marshaler needs, e.g. an `f32` → xmm).
        let mut pstmt = self.conn.prepare(
            "SELECT p.method_id, p.type_name FROM interface_method_params p
               JOIN interface_methods m ON m.method_id = p.method_id
               WHERE m.interface_type_id = ?1 ORDER BY p.method_id, p.ordinal",
        )?;
        let mut params_by_method: std::collections::HashMap<i64, Vec<String>> =
            std::collections::HashMap::new();
        for row in pstmt.query_map([tid], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
            let (mid, tn) = row?;
            params_by_method.entry(mid).or_default().push(tn);
        }
        let mut stmt = self.conn.prepare(
            "SELECT method_id, vtable_index, method_name FROM interface_methods
               WHERE interface_type_id = ?1 ORDER BY vtable_index",
        )?;
        let methods = stmt
            .query_map([tid], |r| {
                let mid: i64 = r.get(0)?;
                Ok(Method {
                    vtable_index: r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    name: r.get(2)?,
                    params: params_by_method.get(&mid).cloned().unwrap_or_default(),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Some(Interface { name: name.to_string(), iid, base, methods }))
    }
}

/// A struct/union field with its computed byte offset.
#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub offset: i64,
    pub type_name: String,
}

/// A struct/union layout (sizes in bytes).
#[derive(Debug, Clone)]
pub struct Layout {
    pub name: String,
    pub kind: String,
    pub size: i64,
    pub align: i64,
    pub fields: Vec<Field>,
}

/// A COM interface method in vtable order.
#[derive(Debug, Clone)]
pub struct Method {
    pub vtable_index: i64,
    pub name: String,
    /// Parameter type names, by ordinal (the Microsoft data's names are shifted
    /// by one, but the types are aligned — enough to know which args are floats).
    pub params: Vec<String>,
}

/// A COM interface: IID + base + own vtable methods.
#[derive(Debug, Clone)]
pub struct Interface {
    pub name: String,
    pub iid: Option<String>,
    pub base: Option<String>,
    pub methods: Vec<Method>,
}

impl Kb {
    /// Generate a ready-to-edit `invoke` snippet for a function: enum-typed
    /// parameters get a default member (a real, resolvable constant); everything
    /// else becomes a `<paramName>` field for the user to fill.
    pub fn snippet(&self, func: &str) -> Result<Option<String>> {
        let Some(f) = self.function(func)? else {
            return Ok(None);
        };
        let sig: Vec<&str> = f.params.iter().map(|p| p.name.as_str()).collect();
        let args: Vec<String> = f
            .params
            .iter()
            .map(|p| {
                if p.type_kind == "enum" {
                    if let Some((m, _)) = p.related.first() {
                        return m.clone();
                    }
                }
                format!("<{}>", p.name)
            })
            .collect();
        let dll = f.dll.as_deref().unwrap_or("");
        let head = format!("; {} {}({})   [{}]", f.ret, f.name, sig.join(", "), dll);
        let call = if args.is_empty() {
            format!("invoke {}", f.name)
        } else {
            format!("invoke {}, {}", f.name, args.join(", "))
        };
        Ok(Some(format!("{head}\n{call}")))
    }

    /// "Did you mean" — constant/enum names close (edit distance ≤ 3) to `name`,
    /// nearest first. Prefix-indexed so it stays fast over ~165K names.
    pub fn suggest(&self, name: &str, limit: usize) -> Result<Vec<String>> {
        if name.len() < 2 {
            return Ok(Vec::new());
        }
        let plen = name.len().min(4);
        let prefix = format!("{}%", &name[..plen]);
        let mut stmt = self.conn.prepare(
            "SELECT name FROM (
               SELECT constant_name AS name FROM constants WHERE constant_name LIKE ?1
               UNION SELECT member_name FROM enum_members WHERE member_name LIKE ?1
             ) LIMIT 800",
        )?;
        let cands: Vec<String> =
            stmt.query_map([&prefix], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        let mut scored: Vec<(usize, String)> = cands
            .into_iter()
            .map(|c| (levenshtein(name, &c), c))
            .filter(|(d, _)| *d > 0 && *d <= 3)
            .collect();
        scored.sort();
        scored.truncate(limit);
        Ok(scored.into_iter().map(|(_, n)| n).collect())
    }
}

/// Escape SQL `LIKE` metacharacters so a string matches literally (used with
/// `ESCAPE '\'`). Windows names contain `_` constantly, which would otherwise be
/// a single-char wildcard.
fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        if c == '%' || c == '_' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Classic edit distance (small names; used only for "did you mean").
fn levenshtein(a: &str, b: &str) -> usize {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, &ac) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, &bc) in b.iter().enumerate() {
            let cost = if ac == bc { 0 } else { 1 };
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kb() -> Option<Kb> {
        let path = std::env::var("WINKB_DB")
            .unwrap_or_else(|_| r"E:\windows_api\windows_api.db".to_string());
        Kb::open(&path).ok()
    }

    #[test]
    fn complete_function_prefix() {
        let Some(kb) = kb() else { return };
        let hits = kb.complete("CreateFile", "function", 50).unwrap();
        assert!(hits.iter().any(|c| c.name == "CreateFileW"), "{hits:?}");
        assert!(hits.iter().all(|c| c.kind == "function" && c.name.starts_with("CreateFile")));
    }

    #[test]
    fn complete_constant_scope_excludes_functions() {
        let Some(kb) = kb() else { return };
        let hits = kb.complete("FILE_SHARE_", "constant", 50).unwrap();
        assert!(hits.iter().any(|c| c.name == "FILE_SHARE_READ"), "{hits:?}");
        assert!(hits.iter().all(|c| c.kind != "function"));
    }

    #[test]
    fn complete_unknown_scope_is_empty() {
        let Some(kb) = kb() else { return };
        assert!(kb.complete("X", "bogus", 10).unwrap().is_empty());
    }
}
