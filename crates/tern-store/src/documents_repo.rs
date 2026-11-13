//! Documents (docs/documents-design.md): a record, an append-only revision
//! log and content-addressed text. Every write is one transaction that checks
//! the revision it was based on, so a slow writer can never overwrite a newer
//! one. Each create and revision is a ledger entry, on the same connection.

use rusqlite::{OptionalExtension, Row, Transaction, params};
use sha2::{Digest, Sha256};

use crate::{LedgerEntry, Result, Store, now_ms};

/// sha256 hex of `text`: the blob key and the `content_hash` of a revision.
pub fn content_hash(text: &str) -> String {
    let d = Sha256::digest(text.as_bytes());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocumentRow {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub workspace_id: String,
    pub path: String,
    pub current_rev: i64,
    pub content_hash: String,
    pub size: i64,
    pub tainted: bool,
    pub agent_id: Option<String>,
    pub session_id: Option<String>,
    pub run_id: Option<String>,
    pub owner_user_id: Option<String>,
    pub visibility: String,
    pub created_by: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RevisionRow {
    pub doc_id: String,
    pub rev: i64,
    pub parent_rev: Option<i64>,
    /// `agent`, `user`, `external` or `system`.
    pub author_kind: String,
    pub author_id: String,
    pub run_id: Option<String>,
    pub tool_call_id: Option<String>,
    pub size: i64,
    pub added: i64,
    pub removed: i64,
    pub summary: Option<String>,
    pub tainted: bool,
    pub provenance: Option<String>,
    pub rebased: bool,
    pub created_at: i64,
}

/// A document to create, with its first revision's text.
#[derive(Debug, Clone, Default)]
pub struct NewDocument<'a> {
    pub id: &'a str,
    pub kind: &'a str,
    pub title: &'a str,
    pub workspace_id: &'a str,
    pub path: &'a str,
    pub content: &'a str,
    pub agent_id: Option<&'a str>,
    pub session_id: Option<&'a str>,
    pub run_id: Option<&'a str>,
    pub tool_call_id: Option<&'a str>,
    /// `agent`, `user`, `external` or `system`.
    pub author_kind: &'a str,
    pub author_id: &'a str,
    pub summary: Option<&'a str>,
    pub tainted: bool,
    pub provenance: Option<&'a str>,
    pub added: i64,
}

/// A revision to append to a document.
#[derive(Debug, Clone, Default)]
pub struct NewRevision<'a> {
    pub doc_id: &'a str,
    /// The revision the caller's text is based on: the write is refused if
    /// the document has moved on since.
    pub expect_rev: i64,
    pub content: &'a str,
    pub author_kind: &'a str,
    pub author_id: &'a str,
    pub run_id: Option<&'a str>,
    pub session_id: Option<&'a str>,
    pub tool_call_id: Option<&'a str>,
    pub summary: Option<&'a str>,
    pub tainted: bool,
    pub provenance: Option<&'a str>,
    pub rebased: bool,
    pub added: i64,
    pub removed: i64,
}

/// What a write came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Commit {
    Done(Box<DocumentRow>, Box<RevisionRow>),
    /// The document is at another revision than the write expected.
    Stale {
        current_rev: i64,
    },
    /// A live document already has that path in that workspace.
    Exists {
        id: String,
    },
    /// No such document.
    Missing,
}

#[derive(Debug, Clone, Default)]
pub struct DocumentFilter<'a> {
    pub session_id: Option<&'a str>,
    pub workspace_id: Option<&'a str>,
    /// A substring of the title or path.
    pub query: Option<&'a str>,
    pub limit: Option<usize>,
}

const COLS: &str = "id, kind, title, workspace_id, path, current_rev, content_hash, size, tainted,
    agent_id, session_id, run_id, owner_user_id, visibility, created_by, created_at, updated_at";

fn doc_row(r: &Row<'_>) -> rusqlite::Result<DocumentRow> {
    Ok(DocumentRow {
        id: r.get(0)?,
        kind: r.get(1)?,
        title: r.get(2)?,
        workspace_id: r.get(3)?,
        path: r.get(4)?,
        current_rev: r.get(5)?,
        content_hash: r.get(6)?,
        size: r.get(7)?,
        tainted: r.get(8)?,
        agent_id: r.get(9)?,
        session_id: r.get(10)?,
        run_id: r.get(11)?,
        owner_user_id: r.get(12)?,
        visibility: r.get(13)?,
        created_by: r.get(14)?,
        created_at: r.get(15)?,
        updated_at: r.get(16)?,
    })
}

const REV_COLS: &str = "doc_id, rev, parent_rev, author_kind, author_id, run_id, tool_call_id,
    size, added, removed, summary, tainted, provenance, rebased, created_at";

fn rev_row(r: &Row<'_>) -> rusqlite::Result<RevisionRow> {
    Ok(RevisionRow {
        doc_id: r.get(0)?,
        rev: r.get(1)?,
        parent_rev: r.get(2)?,
        author_kind: r.get(3)?,
        author_id: r.get(4)?,
        run_id: r.get(5)?,
        tool_call_id: r.get(6)?,
        size: r.get(7)?,
        added: r.get(8)?,
        removed: r.get(9)?,
        summary: r.get(10)?,
        tainted: r.get(11)?,
        provenance: r.get(12)?,
        rebased: r.get(13)?,
        created_at: r.get(14)?,
    })
}

/// The ledger actor for a revision's author.
fn actor(kind: &str, id: &str) -> String {
    match kind {
        "agent" => format!("agent:{id}"),
        "user" => crate::ACTOR_OWNER.to_owned(),
        _ => crate::ACTOR_TERN.to_owned(),
    }
}

fn put_blob(tx: &Transaction<'_>, hash: &str, content: &str) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO document_blobs (hash, content) VALUES (?1, ?2)",
        params![hash, content],
    )?;
    Ok(())
}

impl Store {
    /// Create a document and its first revision, unless a live document has
    /// the path already.
    pub fn create_document(&self, d: &NewDocument<'_>) -> Result<Commit> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        if let Some(id) = tx
            .query_row(
                "SELECT id FROM documents WHERE workspace_id = ?1 AND path = ?2
                   AND deleted_at IS NULL",
                params![d.workspace_id, d.path],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            return Ok(Commit::Exists { id });
        }
        let now = now_ms();
        let hash = content_hash(d.content);
        let size = d.content.len() as i64;
        put_blob(&tx, &hash, d.content)?;
        tx.execute(
            "INSERT INTO documents (id, kind, title, workspace_id, path, current_rev,
               content_hash, size, tainted, agent_id, session_id, run_id, visibility,
               created_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, ?8, ?9, ?10, ?11, 'private', ?12, ?13, ?13)",
            params![
                d.id,
                d.kind,
                d.title,
                d.workspace_id,
                d.path,
                hash,
                size,
                d.tainted,
                d.agent_id,
                d.session_id,
                d.run_id,
                actor(d.author_kind, d.author_id),
                now
            ],
        )?;
        tx.execute(
            "INSERT INTO document_revisions (doc_id, rev, parent_rev, author_kind, author_id,
               run_id, tool_call_id, blob_hash, size, added, removed, summary, tainted,
               provenance, rebased, created_at)
             VALUES (?1, 1, NULL, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9, ?10, ?11, 0, ?12)",
            params![
                d.id,
                d.author_kind,
                d.author_id,
                d.run_id,
                d.tool_call_id,
                hash,
                size,
                d.added,
                d.summary,
                d.tainted,
                d.provenance,
                now
            ],
        )?;
        let mut e = LedgerEntry::new(
            "document.create",
            d.id,
            format!("created document {}", d.path),
        )
        .actor(&actor(d.author_kind, d.author_id))
        .detail(serde_json::json!({
            "rev": 1, "hash": hash, "bytes": size, "path": d.path,
            "workspace": d.workspace_id, "lines": d.added, "tainted": d.tainted,
        }));
        if let (Some(r), Some(s)) = (d.run_id, d.session_id) {
            e = e.run(r, s);
        }
        crate::ledger_repo::append_on(&tx, &e)?;
        tx.commit()?;
        drop(conn);
        match (self.document(d.id)?, self.revision(d.id, 1)?) {
            (Some(doc), Some(rev)) => Ok(Commit::Done(Box::new(doc), Box::new(rev))),
            _ => Ok(Commit::Missing),
        }
    }

    /// Append a revision. `Stale` when the document is not at `expect_rev`.
    pub fn commit_revision(&self, n: &NewRevision<'_>) -> Result<Commit> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let Some((current, path, was_tainted)) = tx
            .query_row(
                "SELECT current_rev, path, tainted FROM documents
                 WHERE id = ?1 AND deleted_at IS NULL",
                [n.doc_id],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, bool>(2)?,
                    ))
                },
            )
            .optional()?
        else {
            return Ok(Commit::Missing);
        };
        if current != n.expect_rev {
            return Ok(Commit::Stale {
                current_rev: current,
            });
        }
        let rev = current + 1;
        let now = now_ms();
        let hash = content_hash(n.content);
        let size = n.content.len() as i64;
        put_blob(&tx, &hash, n.content)?;
        tx.execute(
            "INSERT INTO document_revisions (doc_id, rev, parent_rev, author_kind, author_id,
               run_id, tool_call_id, blob_hash, size, added, removed, summary, tainted,
               provenance, rebased, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                n.doc_id,
                rev,
                current,
                n.author_kind,
                n.author_id,
                n.run_id,
                n.tool_call_id,
                hash,
                size,
                n.added,
                n.removed,
                n.summary,
                n.tainted,
                n.provenance,
                n.rebased,
                now
            ],
        )?;
        tx.execute(
            "UPDATE documents SET current_rev = ?2, content_hash = ?3, size = ?4,
               tainted = ?5, updated_at = ?6 WHERE id = ?1",
            params![n.doc_id, rev, hash, size, was_tainted || n.tainted, now],
        )?;
        let mut e = LedgerEntry::new(
            "document.revise",
            n.doc_id,
            format!(
                "revised document {path}: rev {rev} (+{} -{})",
                n.added, n.removed
            ),
        )
        .actor(&actor(n.author_kind, n.author_id))
        .detail(serde_json::json!({
            "rev": rev, "parent_rev": current, "hash": hash, "bytes": size, "path": path,
            "added": n.added, "removed": n.removed, "rebased": n.rebased, "tainted": n.tainted,
        }));
        if let (Some(r), Some(s)) = (n.run_id, n.session_id) {
            e = e.run(r, s);
        }
        crate::ledger_repo::append_on(&tx, &e)?;
        tx.commit()?;
        drop(conn);
        match (self.document(n.doc_id)?, self.revision(n.doc_id, rev)?) {
            (Some(doc), Some(r)) => Ok(Commit::Done(Box::new(doc), Box::new(r))),
            _ => Ok(Commit::Missing),
        }
    }

    pub fn document(&self, id: &str) -> Result<Option<DocumentRow>> {
        let sql = format!("SELECT {COLS} FROM documents WHERE id = ?1 AND deleted_at IS NULL");
        Ok(self.conn()?.query_row(&sql, [id], doc_row).optional()?)
    }

    pub fn document_by_path(&self, workspace_id: &str, path: &str) -> Result<Option<DocumentRow>> {
        let sql = format!(
            "SELECT {COLS} FROM documents
             WHERE workspace_id = ?1 AND path = ?2 AND deleted_at IS NULL"
        );
        Ok(self
            .conn()?
            .query_row(&sql, params![workspace_id, path], doc_row)
            .optional()?)
    }

    /// Newest first.
    pub fn documents(&self, f: &DocumentFilter<'_>) -> Result<Vec<DocumentRow>> {
        let sql = format!(
            "SELECT {COLS} FROM documents
             WHERE deleted_at IS NULL
               AND (?1 IS NULL OR session_id = ?1)
               AND (?2 IS NULL OR workspace_id = ?2)
               AND (?3 IS NULL OR instr(lower(title || ' ' || path), lower(?3)) > 0)
             ORDER BY updated_at DESC, id DESC LIMIT ?4"
        );
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![
                f.session_id,
                f.workspace_id,
                f.query,
                f.limit.unwrap_or(200) as i64
            ],
            doc_row,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Newest first.
    pub fn revisions(&self, doc_id: &str) -> Result<Vec<RevisionRow>> {
        let sql = format!(
            "SELECT {REV_COLS} FROM document_revisions WHERE doc_id = ?1 ORDER BY rev DESC"
        );
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([doc_id], rev_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn revision(&self, doc_id: &str, rev: i64) -> Result<Option<RevisionRow>> {
        let sql =
            format!("SELECT {REV_COLS} FROM document_revisions WHERE doc_id = ?1 AND rev = ?2");
        Ok(self
            .conn()?
            .query_row(&sql, params![doc_id, rev], rev_row)
            .optional()?)
    }

    /// The text of one revision.
    pub fn revision_content(&self, doc_id: &str, rev: i64) -> Result<Option<String>> {
        Ok(self
            .conn()?
            .query_row(
                "SELECT b.content FROM document_revisions r
                 JOIN document_blobs b ON b.hash = r.blob_hash
                 WHERE r.doc_id = ?1 AND r.rev = ?2",
                params![doc_id, rev],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// How many revisions (creates included) a run has written.
    pub fn document_revisions_by_run(&self, run_id: &str) -> Result<i64> {
        Ok(self.conn()?.query_row(
            "SELECT count(*) FROM document_revisions WHERE run_id = ?1 AND author_kind = 'agent'",
            [run_id],
            |r| r.get(0),
        )?)
    }

    /// Mark a document as reviewed: clears the taint flag from now on.
    pub fn mark_document_reviewed(&self, id: &str) -> Result<bool> {
        let conn = self.conn()?;
        let n = conn.execute(
            "UPDATE documents SET tainted = 0 WHERE id = ?1 AND deleted_at IS NULL",
            [id],
        )?;
        if n > 0 {
            let e = LedgerEntry::new("document.review", id, "marked a document as reviewed");
            crate::ledger_repo::append_on(&conn, &e)?;
        }
        Ok(n > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_doc<'a>(id: &'a str, path: &'a str, content: &'a str) -> NewDocument<'a> {
        NewDocument {
            id,
            kind: "markdown",
            title: "T",
            workspace_id: "w",
            path,
            content,
            author_kind: "agent",
            author_id: "tern",
            ..NewDocument::default()
        }
    }

    #[test]
    fn create_then_revise_keeps_every_revision_and_checks_the_base() {
        let s = Store::open_in_memory().unwrap();
        let Commit::Done(d, r) = s
            .create_document(&new_doc("d1", "documents/a.md", "one"))
            .unwrap()
        else {
            panic!("created");
        };
        assert_eq!((d.current_rev, r.rev), (1, 1));
        let rev = |base, text: &'static str| NewRevision {
            doc_id: "d1",
            expect_rev: base,
            content: text,
            author_kind: "user",
            author_id: "owner",
            ..NewRevision::default()
        };
        let Commit::Done(d, _) = s.commit_revision(&rev(1, "two")).unwrap() else {
            panic!("rev 2");
        };
        assert_eq!(d.current_rev, 2);
        // A write based on rev 1 is now stale and writes nothing.
        assert_eq!(
            s.commit_revision(&rev(1, "three")).unwrap(),
            Commit::Stale { current_rev: 2 }
        );
        assert_eq!(s.revisions("d1").unwrap().len(), 2);
        assert_eq!(s.revision_content("d1", 1).unwrap().as_deref(), Some("one"));
        assert_eq!(s.revision_content("d1", 2).unwrap().as_deref(), Some("two"));
    }

    #[test]
    fn a_live_path_cannot_be_created_twice() {
        let s = Store::open_in_memory().unwrap();
        s.create_document(&new_doc("d1", "documents/a.md", "x"))
            .unwrap();
        assert_eq!(
            s.create_document(&new_doc("d2", "documents/a.md", "y"))
                .unwrap(),
            Commit::Exists { id: "d1".into() }
        );
    }

    #[test]
    fn taint_sticks_until_reviewed_and_every_write_is_in_the_ledger() {
        let s = Store::open_in_memory().unwrap();
        let mut d = new_doc("d1", "documents/a.md", "x");
        d.tainted = true;
        d.provenance = Some("web_fetch");
        s.create_document(&d).unwrap();
        let clean = NewRevision {
            doc_id: "d1",
            expect_rev: 1,
            content: "y",
            author_kind: "user",
            author_id: "owner",
            ..NewRevision::default()
        };
        s.commit_revision(&clean).unwrap();
        assert!(
            s.document("d1").unwrap().unwrap().tainted,
            "a clean edit does not clear it"
        );
        assert!(s.mark_document_reviewed("d1").unwrap());
        assert!(!s.document("d1").unwrap().unwrap().tainted);
        let kinds: Vec<_> = s
            .ledger(&crate::LedgerFilter::default())
            .unwrap()
            .into_iter()
            .map(|r| r.entry.kind)
            .collect();
        assert!(kinds.iter().any(|k| k == "document.create"));
        assert!(kinds.iter().any(|k| k == "document.revise"));
    }
}
