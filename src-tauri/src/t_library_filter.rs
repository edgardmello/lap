//! Library filters and keywords share the catalog query predicates, including
//! paginated files, counts, groups and timelines. No client-side truncation.
use crate::t_sqlite::{self, AFile, ATagFileState, QueryParams, SmartQueryParams};
use rusqlite::{Connection, ToSql, params, params_from_iter};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LibraryFilter {
    pub text: String,
    pub field: String,
    pub mode: String,
    pub rating_min: Option<i64>,
    pub flag: Option<i64>,
    pub favorite: bool,
    pub years: Vec<String>,
    pub cameras: Vec<String>,
    pub lenses: Vec<String>,
    pub labels: Vec<String>,
    pub keyword_ids: Vec<i64>,
    pub without_keywords: bool,
    pub file_types: Vec<String>,
}

const YEAR: &str = "COALESCE(strftime('%Y',NULLIF(a.taken_date,0),'unixepoch','localtime'),'')";
const CAMERA: &str = "TRIM(COALESCE(a.e_make,'') || ' ' || COALESCE(a.e_model,''))";
const LENS: &str = "TRIM(COALESCE(a.e_lens_make,'') || ' ' || COALESCE(a.e_lens_model,''))";
const LABEL: &str = "COALESCE((SELECT label FROM afile_labels fl WHERE fl.file_id=a.id),'')";
const FORMAT: &str = "UPPER(COALESCE(NULLIF(a.format_label,''),CASE a.file_type WHEN 3 THEN 'RAW' WHEN 2 THEN 'VIDEO' ELSE 'IMAGE' END))";

fn like_literal(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}
fn in_values(
    expr: &str,
    values: &[String],
    conditions: &mut Vec<String>,
    bindings: &mut Vec<Box<dyn ToSql>>,
) {
    if values.is_empty() {
        return;
    }
    conditions.push(format!("{expr} IN ({})", vec!["?"; values.len()].join(",")));
    bindings.extend(
        values
            .iter()
            .cloned()
            .map(|v| Box::new(v) as Box<dyn ToSql>),
    );
}

pub(crate) fn append_filter(
    f: &LibraryFilter,
    conditions: &mut Vec<String>,
    bindings: &mut Vec<Box<dyn ToSql>>,
) {
    let text = f.text.trim();
    if !text.is_empty() {
        let keywords = "COALESCE((SELECT group_concat(t.name,' ') FROM atags t JOIN afile_tags ft ON t.id=ft.tag_id WHERE ft.file_id=a.id),'')";
        let fields = match f.field.as_str() {
            "filename" => vec!["COALESCE(a.name,'')".to_string()],
            "keywords" => vec![keywords.to_string()],
            "description" => vec![
                "COALESCE(a.comments,'')".into(),
                "COALESCE(a.e_description,'')".into(),
            ],
            _ => vec![
                "COALESCE(a.name,'')".into(),
                "COALESCE(a.comments,'')".into(),
                "COALESCE(a.e_description,'')".into(),
                CAMERA.into(),
                LENS.into(),
                keywords.into(),
            ],
        };
        let terms: Vec<&str> = if matches!(f.mode.as_str(), "all" | "any") {
            text.split_whitespace().take(32).collect()
        } else {
            vec![text]
        };
        let mut term_conditions = Vec::new();
        for term in terms {
            let escaped = like_literal(term);
            let pattern = match f.mode.as_str() {
                "starts" => format!("{escaped}%"),
                "ends" => format!("%{escaped}"),
                _ => format!("%{escaped}%"),
            };
            term_conditions.push(format!(
                "({})",
                fields
                    .iter()
                    .map(|field| format!("{field} LIKE ? ESCAPE '\\' COLLATE NOCASE"))
                    .collect::<Vec<_>>()
                    .join(" OR ")
            ));
            for _ in &fields {
                bindings.push(Box::new(pattern.clone()));
            }
        }
        let predicate = term_conditions.join(if f.mode == "any" { " OR " } else { " AND " });
        conditions.push(if f.mode == "not" {
            format!("NOT ({predicate})")
        } else {
            format!("({predicate})")
        });
    }
    if let Some(rating) = f.rating_min {
        conditions.push("COALESCE(a.rating,0) >= ?".into());
        bindings.push(Box::new(rating.clamp(0, 5)));
    }
    if let Some(flag) = f.flag {
        conditions.push("COALESCE(a.culling_flag,0) = ?".into());
        bindings.push(Box::new(flag));
    }
    if f.favorite {
        conditions.push("a.is_favorite = 1".into());
    }
    in_values(YEAR, &f.years, conditions, bindings);
    in_values(CAMERA, &f.cameras, conditions, bindings);
    in_values(LENS, &f.lenses, conditions, bindings);
    in_values(LABEL, &f.labels, conditions, bindings);
    in_values(FORMAT, &f.file_types, conditions, bindings);
    if !f.keyword_ids.is_empty() {
        let choices = vec!["(parent.id=? AND (child.id=parent.id OR substr(child.name,1,length(parent.name)+3)=parent.name || ' > '))"; f.keyword_ids.len()].join(" OR ");
        conditions.push(format!("EXISTS (SELECT 1 FROM afile_tags ft JOIN atags child ON child.id=ft.tag_id CROSS JOIN atags parent WHERE ft.file_id=a.id AND ({choices}))"));
        bindings.extend(
            f.keyword_ids
                .iter()
                .map(|id| Box::new(*id) as Box<dyn ToSql>),
        );
    }
    if f.without_keywords {
        conditions.push("NOT EXISTS (SELECT 1 FROM afile_tags ft WHERE ft.file_id=a.id)".into());
    }
}

#[derive(Debug, Serialize)]
pub struct FacetValue {
    value: String,
    count: i64,
}
#[derive(Debug, Serialize)]
pub struct FacetColumn {
    values: Vec<FacetValue>,
    total: i64,
}

#[tauri::command]
pub async fn get_library_facets(
    params: QueryParams,
    smart_params: Option<SmartQueryParams>,
    collection_id: Option<i64>,
) -> Result<HashMap<String, FacetColumn>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = t_sqlite::open_conn()?;
        facets(&conn, &params, smart_params.as_ref(), collection_id)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn facets(
    conn: &Connection,
    params: &QueryParams,
    smart: Option<&SmartQueryParams>,
    collection: Option<i64>,
) -> Result<HashMap<String, FacetColumn>, String> {
    let mut result = HashMap::new();
    for (key, expr) in [
        ("years", YEAR),
        ("cameras", CAMERA),
        ("lenses", LENS),
        ("labels", LABEL),
        ("fileTypes", FORMAT),
        ("keywordIds", "CAST(kt.id AS TEXT)"),
    ] {
        let mut p = params.clone();
        let mut s = smart.cloned();
        let f = if let Some(s) = s.as_mut() {
            &mut s.library_filter
        } else {
            &mut p.library_filter
        };
        match key {
            "years" => f.years.clear(),
            "cameras" => f.cameras.clear(),
            "lenses" => f.lenses.clear(),
            "labels" => f.labels.clear(),
            "fileTypes" => f.file_types.clear(),
            _ => {
                f.keyword_ids.clear();
                f.without_keywords = false;
            }
        }
        let (joins, clause, mut values) = if let Some(s) = s.as_ref() {
            let (j, w, v, _) = AFile::build_smart_query_parts(s)?;
            (j, w, v)
        } else {
            AFile::build_search_query_parts(&p)
        };
        let collection_clause = if let Some(id) = collection {
            values.push(Box::new(id));
            " AND EXISTS (SELECT 1 FROM acollections_files cf WHERE cf.file_id=a.id AND cf.collection_id=?)"
        } else {
            ""
        };
        let keyword_join = if key == "keywordIds" {
            " LEFT JOIN afile_tags kft ON kft.file_id=a.id LEFT JOIN atags child ON child.id=kft.tag_id LEFT JOIN atags kt ON child.id=kt.id OR substr(child.name,1,length(kt.name)+3)=kt.name || ' > '"
        } else {
            ""
        };
        let sql = format!(
            "SELECT COALESCE({expr},''),COUNT(DISTINCT a.id) FROM afiles a LEFT JOIN afolders b ON b.id=a.folder_id LEFT JOIN albums c ON c.id=b.album_id {joins} {keyword_join} {clause} {collection_clause} GROUP BY 1 ORDER BY 1 COLLATE NOCASE"
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params_from_iter(values.iter().map(|v| v.as_ref())), |r| {
                Ok(FacetValue {
                    value: r.get(0)?,
                    count: r.get(1)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let options = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        let total=conn.query_row(&format!("SELECT COUNT(DISTINCT a.id) FROM afiles a LEFT JOIN afolders b ON b.id=a.folder_id LEFT JOIN albums c ON c.id=b.album_id {joins} {clause} {collection_clause}"),params_from_iter(values.iter().map(|v|v.as_ref())),|r|r.get(0)).map_err(|e|e.to_string())?;
        result.insert(
            key.into(),
            FacetColumn {
                values: options,
                total,
            },
        );
    }
    Ok(result)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Keyword {
    id: i64,
    name: String,
    group_name: String,
    count: i64,
    selected_count: i64,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeywordPanel {
    keywords: Vec<Keyword>,
    suggestions: Vec<i64>,
    selection_count: usize,
}

fn populate_selection(conn: &Connection, ids: &[i64]) -> Result<(), String> {
    conn.execute_batch("CREATE TEMP TABLE IF NOT EXISTS keyword_selection(id INTEGER PRIMARY KEY); DELETE FROM keyword_selection;").map_err(|e|e.to_string())?;
    let mut stmt = conn
        .prepare("INSERT OR IGNORE INTO keyword_selection VALUES (?)")
        .map_err(|e| e.to_string())?;
    for id in ids {
        stmt.execute([id]).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn get_keyword_panel(file_ids: Vec<i64>) -> Result<KeywordPanel, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut conn = t_sqlite::open_conn()?;
        keyword_panel_on(&mut conn, &file_ids)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn keyword_panel_on(conn: &mut Connection, file_ids: &[i64]) -> Result<KeywordPanel, String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    populate_selection(&tx, &file_ids)?;
    let visible = format!(
        "{} AND {} AND {}",
        AFile::album_query_predicate("a"),
        AFile::search_exclusion_condition("b"),
        AFile::live_photo_companion_exclusion_condition()
    );
    let keywords = {
        let mut stmt=tx.prepare(&format!("WITH RECURSIVE paths(tag_id,path,rest) AS (
              SELECT id,'',name || ' > ' FROM atags UNION ALL
              SELECT tag_id,CASE WHEN path='' THEN substr(rest,1,instr(rest,' > ')-1) ELSE path || ' > ' || substr(rest,1,instr(rest,' > ')-1) END,
                substr(rest,instr(rest,' > ')+3) FROM paths WHERE rest<>'' AND instr(rest,' > ')>0
            ), ancestors AS (SELECT paths.tag_id,parent.id FROM paths JOIN atags parent ON parent.name=paths.path),
            expanded AS (SELECT DISTINCT ft.file_id,parent.id tag_id FROM afile_tags ft JOIN ancestors parent ON parent.tag_id=ft.tag_id
              JOIN afiles a ON a.id=ft.file_id JOIN afolders b ON b.id=a.folder_id WHERE {visible})
            SELECT t.id,t.name,COALESCE(g.name,''),COUNT(DISTINCT e.file_id),COUNT(DISTINCT s.id)
            FROM atags t LEFT JOIN atag_groups g ON g.id=t.group_id LEFT JOIN expanded e ON e.tag_id=t.id LEFT JOIN keyword_selection s ON s.id=e.file_id
            GROUP BY t.id ORDER BY t.name COLLATE NOCASE")).map_err(|e|e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(Keyword {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    group_name: r.get(2)?,
                    count: r.get(3)?,
                    selected_count: r.get(4)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
    };
    let suggestions = {
        // Related photos share an assigned keyword, a folder, or a nearby
        // capture time. Rank related-photo frequency, then library frequency.
        let mut stmt=tx.prepare(&format!("SELECT t.id FROM atags t JOIN afile_tags ft ON ft.tag_id=t.id JOIN afiles related ON related.id=ft.file_id
              WHERE EXISTS(SELECT 1 FROM keyword_selection)
              AND related.id IN (SELECT a.id FROM afiles a JOIN afolders b ON b.id=a.folder_id WHERE {visible})
              AND NOT EXISTS(SELECT 1 FROM keyword_selection s JOIN afile_tags present ON present.file_id=s.id WHERE present.tag_id=t.id)
              AND EXISTS(SELECT 1 FROM keyword_selection s JOIN afiles chosen ON chosen.id=s.id
                WHERE chosen.folder_id=related.folder_id OR (chosen.taken_date>0 AND related.taken_date BETWEEN chosen.taken_date-86400 AND chosen.taken_date+86400)
                OR EXISTS(SELECT 1 FROM afile_tags a JOIN afile_tags b ON a.tag_id=b.tag_id WHERE a.file_id=chosen.id AND b.file_id=related.id))
              GROUP BY t.id ORDER BY COUNT(DISTINCT related.id) DESC,t.name COLLATE NOCASE LIMIT 12")).map_err(|e|e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<i64>, _>>()
            .map_err(|e| e.to_string())?
    };
    let selection_count = tx
        .query_row("SELECT COUNT(*) FROM keyword_selection", [], |r| {
            r.get::<_, usize>(0)
        })
        .map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM keyword_selection", [])
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(KeywordPanel {
        keywords,
        suggestions,
        selection_count,
    })
}

#[tauri::command]
pub async fn add_keywords(
    file_ids: Vec<i64>,
    names: Vec<String>,
) -> Result<Vec<ATagFileState>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut conn = t_sqlite::open_conn()?;
        add_keywords_on(&mut conn, &file_ids, &names)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn add_keywords_on(
    conn: &mut Connection,
    file_ids: &[i64],
    names: &[String],
) -> Result<Vec<ATagFileState>, String> {
    if names.len() > 100 {
        return Err("At most 100 keywords per action".into());
    }
    let paths = names
        .iter()
        .map(|name| {
            name.split('>')
                .map(str::trim)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    if paths
        .iter()
        .any(|p| p.iter().any(|s| s.is_empty()) || p.join(" > ").chars().count() > 255)
    {
        return Err("Keywords must contain 1–255 characters and nonempty hierarchy levels".into());
    }
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let group: i64 = tx
        .query_row("SELECT id FROM atag_groups WHERE is_default=1", [], |r| {
            r.get(0)
        })
        .map_err(|e| e.to_string())?;
    let mut leaf_ids = Vec::new();
    for path in paths {
        let mut canonical = String::new();
        for depth in 1..=path.len() {
            let name = if canonical.is_empty() {
                path[depth - 1].clone()
            } else {
                format!("{canonical} > {}", path[depth - 1])
            };
            tx.execute("INSERT INTO atags(name,group_id) SELECT ?1,?2 WHERE NOT EXISTS(SELECT 1 FROM atags WHERE name=?1 COLLATE NOCASE)",params![name,group]).map_err(|e|e.to_string())?;
            let (id, stored) = tx
                .query_row(
                    "SELECT id,name FROM atags WHERE name=? COLLATE NOCASE",
                    [name],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
                )
                .map_err(|e| e.to_string())?;
            canonical = stored;
            if depth == path.len() {
                leaf_ids.push(id);
            }
        }
    }
    populate_selection(&tx, &file_ids)?;
    for id in leaf_ids {
        tx.execute(
            "INSERT OR IGNORE INTO afile_tags(file_id,tag_id) SELECT id,? FROM keyword_selection",
            [id],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.execute("UPDATE afiles SET has_tags=EXISTS(SELECT 1 FROM afile_tags ft WHERE ft.file_id=afiles.id) WHERE id IN (SELECT id FROM keyword_selection)",[]).map_err(|e|e.to_string())?;
    let states = {
        let mut stmt = tx
            .prepare(
                "SELECT id,has_tags FROM afiles WHERE id IN (SELECT id FROM keyword_selection)",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(ATagFileState {
                    file_id: r.get(0)?,
                    has_tags: r.get(1)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
    };
    tx.execute("DELETE FROM keyword_selection", [])
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(states)
}

#[tauri::command]
pub async fn set_library_color_label(file_ids: Vec<i64>, label: String) -> Result<(), String> {
    if !["", "red", "yellow", "green", "blue", "purple"].contains(&label.as_str()) {
        return Err("Invalid color label".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let mut conn = t_sqlite::open_conn()?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        populate_selection(&tx, &file_ids)?;
        tx.execute(
            "DELETE FROM afile_labels WHERE file_id IN (SELECT id FROM keyword_selection)",
            [],
        )
        .map_err(|e| e.to_string())?;
        if !label.is_empty() {
            tx.execute(
                "INSERT INTO afile_labels(file_id,label) SELECT id,? FROM keyword_selection",
                [label],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.execute("DELETE FROM keyword_selection", [])
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;
          CREATE TABLE albums(id INTEGER PRIMARY KEY,path TEXT,file_types INTEGER,small_image_filter INTEGER,excluded_folders TEXT);
          INSERT INTO albums VALUES(1,'/photos',7,0,'[]');
          CREATE TABLE afolders(id INTEGER PRIMARY KEY,album_id INTEGER,path TEXT,is_excluded_from_search INTEGER);
          INSERT INTO afolders VALUES(1,1,'/photos',0),(2,1,'/photos/hidden',1);
          CREATE TABLE afiles(id INTEGER PRIMARY KEY,folder_id INTEGER,name TEXT,comments TEXT DEFAULT '',e_description TEXT DEFAULT '',
            e_make TEXT DEFAULT '',e_model TEXT DEFAULT '',e_lens_make TEXT DEFAULT '',e_lens_model TEXT DEFAULT '',taken_date INTEGER DEFAULT 1767225600,
            rating INTEGER DEFAULT 0,culling_flag INTEGER DEFAULT 0,is_favorite INTEGER DEFAULT 0,format_label TEXT DEFAULT 'JPEG',file_type INTEGER DEFAULT 1,
            live_photo_video_id INTEGER,width INTEGER DEFAULT 1000,height INTEGER DEFAULT 1000,has_tags INTEGER DEFAULT 0);
          INSERT INTO afiles(id,folder_id,name,e_make,rating) VALUES(1,1,'living room.jpg','Canon',5),(2,1,'kitchen.jpg','Nikon',3),(3,1,'100%_real.jpg','Canon',0),(4,2,'hidden.jpg','Canon',5);
          CREATE TABLE atag_groups(id INTEGER PRIMARY KEY,name TEXT,is_default INTEGER); INSERT INTO atag_groups VALUES(1,'Keywords',1);
          CREATE TABLE atags(id INTEGER PRIMARY KEY,name TEXT UNIQUE,group_id INTEGER);
          CREATE TABLE afile_tags(file_id INTEGER REFERENCES afiles(id),tag_id INTEGER REFERENCES atags(id),PRIMARY KEY(file_id,tag_id));
          CREATE TABLE afile_labels(file_id INTEGER PRIMARY KEY REFERENCES afiles(id) ON DELETE CASCADE,label TEXT);
          INSERT INTO afile_labels VALUES(1,'red'),(2,'blue');
          CREATE TABLE acollections_files(collection_id INTEGER,file_id INTEGER);
          INSERT INTO acollections_files VALUES(7,1),(7,3);").unwrap();
        conn
    }
    fn query() -> QueryParams {
        serde_json::from_value(serde_json::json!({
      "searchFileName":"","searchFileType":0,"sortType":0,"sortOrder":0,"searchAllSubfolders":"","searchFolder":"",
      "startDate":0,"endDate":0,"calendarSort":0,"make":"","model":"","lensMake":"","lensModel":"",
      "locationAdmin1":"","locationName":"","isFavorite":false,"rating":-1,"tagId":0,"personId":0
    })).unwrap()
    }
    fn ids(conn: &Connection, p: &QueryParams) -> Vec<i64> {
        let (j, w, v) = AFile::build_search_query_parts(p);
        let mut stmt=conn.prepare(&format!("SELECT DISTINCT a.id FROM afiles a LEFT JOIN afolders b ON b.id=a.folder_id LEFT JOIN albums c ON c.id=b.album_id {j}{w} ORDER BY a.id")).unwrap();
        stmt.query_map(params_from_iter(v.iter().map(|v| v.as_ref())), |r| r.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }
    #[test]
    fn combines_dimensions_and_escapes_text_literals() {
        let conn = fixture();
        let mut p = query();
        assert_eq!(ids(&conn, &p), vec![1, 2, 3]);
        p.library_filter.labels = vec!["red".into()];
        assert_eq!(ids(&conn, &p), vec![1]);
        p.library_filter.labels.clear();
        p.library_filter.cameras = vec!["Canon".into(), "Nikon".into()];
        p.library_filter.rating_min = Some(4);
        assert_eq!(ids(&conn, &p), vec![1]);
        p.library_filter = LibraryFilter {
            text: "100%_".into(),
            field: "filename".into(),
            ..Default::default()
        };
        assert_eq!(ids(&conn, &p), vec![3]);
        p.library_filter = LibraryFilter {
            text: "room living".into(),
            mode: "all".into(),
            ..Default::default()
        };
        assert_eq!(ids(&conn, &p), vec![1]);
        p.library_filter.mode = "not".into();
        p.library_filter.text = "living".into();
        assert_eq!(ids(&conn, &p), vec![2, 3]);
    }
    #[test]
    fn facets_are_contextual_self_excluding_and_count_distinct_photos() {
        let conn = fixture();
        let mut p = query();
        p.library_filter.cameras = vec!["Canon".into()];
        let f = facets(&conn, &p, None, None).unwrap();
        assert_eq!(f["cameras"].total, 3);
        assert_eq!(f["labels"].total, 2);
        assert_eq!(
            f["cameras"]
                .values
                .iter()
                .find(|v| v.value == "Nikon")
                .unwrap()
                .count,
            1
        );
        let f = facets(&conn, &p, None, Some(7)).unwrap();
        assert_eq!(f["cameras"].total, 2);
        assert!(!f["cameras"].values.iter().any(|v| v.value == "Nikon"));
    }
    #[test]
    fn keyword_hierarchy_is_atomic_deduplicated_and_filters_descendants() {
        let mut conn = fixture();
        add_keywords_on(
            &mut conn,
            &[1, 1],
            &[
                "Properties > Living room".into(),
                "Properties > Kitchen".into(),
            ],
        )
        .unwrap();
        let panel = keyword_panel_on(&mut conn, &[1, 2]).unwrap();
        assert_eq!(panel.keywords.len(), 3);
        let parent = panel
            .keywords
            .iter()
            .find(|k| k.name == "Properties")
            .unwrap();
        assert_eq!((parent.count, parent.selected_count), (1, 1));
        let mut p = query();
        p.library_filter.keyword_ids = vec![parent.id];
        assert_eq!(ids(&conn, &p), vec![1]);
        let f = facets(&conn, &query(), None, None).unwrap();
        assert_eq!(
            f["keywordIds"]
                .values
                .iter()
                .find(|v| v.value == parent.id.to_string())
                .unwrap()
                .count,
            1
        );
        let before = panel.keywords.len();
        add_keywords_on(&mut conn, &[1], &["properties > living room".into()]).unwrap();
        assert_eq!(
            keyword_panel_on(&mut conn, &[]).unwrap().keywords.len(),
            before
        );
        assert!(add_keywords_on(&mut conn, &[999], &["Should roll back".into()]).is_err());
        assert_eq!(
            keyword_panel_on(&mut conn, &[]).unwrap().keywords.len(),
            before
        );
        assert!(add_keywords_on(&mut conn, &[1], &["Properties > > Invalid".into()]).is_err());
        p.library_filter = LibraryFilter {
            without_keywords: true,
            ..Default::default()
        };
        assert_eq!(ids(&conn, &p), vec![2, 3]);
    }
    #[test]
    fn suggestions_use_related_photos_and_exclude_existing_keywords() {
        let mut conn = fixture();
        add_keywords_on(&mut conn, &[1], &["Interior".into()]).unwrap();
        add_keywords_on(&mut conn, &[2], &["Kitchen".into()]).unwrap();
        add_keywords_on(&mut conn, &[4], &["Hidden suggestion".into()]).unwrap();
        let panel = keyword_panel_on(&mut conn, &[1]).unwrap();
        let names = panel
            .suggestions
            .iter()
            .map(|id| {
                panel
                    .keywords
                    .iter()
                    .find(|k| k.id == *id)
                    .unwrap()
                    .name
                    .as_str()
            })
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["Kitchen"]);
        assert!(
            keyword_panel_on(&mut conn, &[])
                .unwrap()
                .suggestions
                .is_empty()
        );
    }
    #[test]
    fn smart_any_rules_still_and_the_library_filter() {
        let conn = fixture();
        let s:SmartQueryParams=serde_json::from_value(serde_json::json!({"match":"any","rules":[{"id":"1","field":"rating","operator":"greaterOrEqual","value":0}],"sortType":0,"sortOrder":0,"libraryFilter":{"labels":["red"]}})).unwrap();
        // Validate filters are trailing AND predicates even with an OR smart query.
        let mut s = s;
        s.rules = vec![
            crate::t_sqlite::SmartRule {
                id: "1".into(),
                field: "favorite".into(),
                operator: "is".into(),
                value: serde_json::json!(false),
            },
            crate::t_sqlite::SmartRule {
                id: "2".into(),
                field: "favorite".into(),
                operator: "is".into(),
                value: serde_json::json!(true),
            },
        ];
        let result = AFile::build_smart_query_parts(&s);
        if let Ok((j, w, v, _)) = result {
            let sql = format!(
                "SELECT a.id FROM afiles a LEFT JOIN afolders b ON b.id=a.folder_id LEFT JOIN albums c ON c.id=b.album_id {j}{w}"
            );
            let mut stmt = conn.prepare(&sql).unwrap();
            let ids = stmt
                .query_map(params_from_iter(v.iter().map(|v| v.as_ref())), |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(ids, vec![1]);
        } else {
            panic!("Smart rule fixture must be supported: {:?}", result.err());
        }
    }
    #[test]
    fn color_label_migration_is_repeatable_and_cascades_without_touching_files() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE afiles(id INTEGER PRIMARY KEY); INSERT INTO afiles VALUES(1); PRAGMA user_version=20;").unwrap();
        crate::t_migration::check_and_migrate(&conn).unwrap();
        crate::t_migration::check_and_migrate(&conn).unwrap();
        conn.execute("INSERT INTO afile_labels VALUES(1,'red')", [])
            .unwrap();
        assert!(
            conn.execute("UPDATE afile_labels SET label='invalid'", [])
                .is_err()
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM afiles", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        conn.execute("DELETE FROM afiles", []).unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM afile_labels", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}
