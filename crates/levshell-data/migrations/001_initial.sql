-- =====================================================
-- Levshell Unified Data Model - Migration 001
-- =====================================================
-- Per-connection pragmas (journal_mode=WAL, foreign_keys=ON,
-- synchronous=NORMAL) are set in DataStore::open before this
-- migration is applied; they are intentionally not duplicated here.

-- -------------------------------------------
-- Projects
-- -------------------------------------------
CREATE TABLE projects (
    id              BLOB(16) PRIMARY KEY,   -- UUID v7
    name            TEXT NOT NULL,
    status          TEXT NOT NULL DEFAULT 'active'
                    CHECK (status IN ('active','simmering',
                           'blocked','writing_up','complete')),
    description     TEXT NOT NULL DEFAULT '',
    open_questions  TEXT NOT NULL DEFAULT '[]',  -- JSON array
    created_at      TEXT NOT NULL,               -- ISO 8601
    updated_at      TEXT NOT NULL
);

-- -------------------------------------------
-- Notes
-- -------------------------------------------
CREATE TABLE notes (
    id              BLOB(16) PRIMARY KEY,
    title           TEXT NOT NULL,
    content         TEXT NOT NULL DEFAULT '',    -- Markdown
    project_id      BLOB(16) REFERENCES projects(id)
                    ON DELETE SET NULL,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);

CREATE INDEX idx_notes_project ON notes(project_id);

-- -------------------------------------------
-- References (literature)
-- -------------------------------------------
CREATE TABLE refs (
    id              BLOB(16) PRIMARY KEY,
    title           TEXT NOT NULL,
    authors         TEXT NOT NULL DEFAULT '[]',  -- JSON array
    year            INTEGER,
    venue           TEXT,
    doi             TEXT,
    citekey         TEXT NOT NULL,
    abstract_text   TEXT,
    pdf_path        TEXT,
    reading_progress REAL DEFAULT 0.0,
    annotations     TEXT NOT NULL DEFAULT '[]',  -- JSON array
    project_id      BLOB(16) REFERENCES projects(id)
                    ON DELETE SET NULL,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);

CREATE UNIQUE INDEX idx_refs_citekey ON refs(citekey);
CREATE INDEX idx_refs_project ON refs(project_id);

-- -------------------------------------------
-- Flashcards
-- -------------------------------------------
CREATE TABLE flashcards (
    id              BLOB(16) PRIMARY KEY,
    front           TEXT NOT NULL,
    back            TEXT NOT NULL,
    linked_note_id  BLOB(16) REFERENCES notes(id)
                    ON DELETE SET NULL,
    linked_ref_id   BLOB(16) REFERENCES refs(id)
                    ON DELETE SET NULL,
    project_id      BLOB(16) REFERENCES projects(id)
                    ON DELETE SET NULL,
    -- SRS scheduling state
    interval_days   REAL NOT NULL DEFAULT 1.0,
    ease_factor     REAL NOT NULL DEFAULT 2.5,
    due_at          TEXT NOT NULL,
    review_count    INTEGER NOT NULL DEFAULT 0,
    last_reviewed   TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);

CREATE INDEX idx_flashcards_due
    ON flashcards(due_at);
CREATE INDEX idx_flashcards_project
    ON flashcards(project_id);

-- -------------------------------------------
-- Events (calendar)
-- -------------------------------------------
CREATE TABLE events (
    id              BLOB(16) PRIMARY KEY,
    title           TEXT NOT NULL,
    start_at        TEXT NOT NULL,
    end_at          TEXT NOT NULL,
    location        TEXT,
    description     TEXT,
    url             TEXT,
    project_id      BLOB(16) REFERENCES projects(id)
                    ON DELETE SET NULL,
    recurrence      TEXT,                    -- iCal RRULE or JSON
    reminders       TEXT NOT NULL DEFAULT '[]',
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);

CREATE INDEX idx_events_start ON events(start_at);
CREATE INDEX idx_events_project ON events(project_id);

-- -------------------------------------------
-- Tasks
-- -------------------------------------------
CREATE TABLE tasks (
    id              BLOB(16) PRIMARY KEY,
    title           TEXT NOT NULL,
    description     TEXT,
    status          TEXT NOT NULL DEFAULT 'pending'
                    CHECK (status IN ('pending','active',
                           'done','cancelled')),
    priority        TEXT CHECK (priority IN ('low','medium',
                           'high','urgent')),
    due_at          TEXT,
    project_id      BLOB(16) REFERENCES projects(id)
                    ON DELETE SET NULL,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);

CREATE INDEX idx_tasks_status ON tasks(status);
CREATE INDEX idx_tasks_due ON tasks(due_at);
CREATE INDEX idx_tasks_project ON tasks(project_id);

-- -------------------------------------------
-- Experiments
-- -------------------------------------------
CREATE TABLE experiments (
    id              BLOB(16) PRIMARY KEY,
    name            TEXT NOT NULL,
    project_id      BLOB(16) NOT NULL
                    REFERENCES projects(id)
                    ON DELETE CASCADE,
    hypothesis      TEXT,
    status          TEXT NOT NULL DEFAULT 'queued'
                    CHECK (status IN ('queued','running',
                           'completed','failed')),
    host            TEXT,
    git_hash        TEXT,
    config          TEXT,                    -- JSON blob
    metrics         TEXT NOT NULL DEFAULT '{}',
    notes           TEXT,
    started_at      TEXT,
    completed_at    TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);

CREATE INDEX idx_experiments_project
    ON experiments(project_id);
CREATE INDEX idx_experiments_status
    ON experiments(status);

-- -------------------------------------------
-- Tags (polymorphic join table)
-- -------------------------------------------
CREATE TABLE entity_tags (
    entity_id       BLOB(16) NOT NULL,
    entity_type     TEXT NOT NULL
                    CHECK (entity_type IN ('project','note',
                           'ref','flashcard','event',
                           'task','experiment')),
    tag             TEXT NOT NULL,
    PRIMARY KEY (entity_id, entity_type, tag)
);

CREATE INDEX idx_tags_by_tag ON entity_tags(tag);
CREATE INDEX idx_tags_by_entity
    ON entity_tags(entity_id, entity_type);

-- -------------------------------------------
-- Entity relations (knowledge/citation graph)
-- -------------------------------------------
CREATE TABLE entity_relations (
    source_id       BLOB(16) NOT NULL,
    source_type     TEXT NOT NULL,
    target_id       BLOB(16) NOT NULL,
    target_type     TEXT NOT NULL,
    relation_kind   TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    PRIMARY KEY (source_id, source_type,
                 target_id, target_type,
                 relation_kind)
);

CREATE INDEX idx_relations_target
    ON entity_relations(target_id, target_type);

-- -------------------------------------------
-- Sync metadata
-- -------------------------------------------
CREATE TABLE sync_metadata (
    entity_id       BLOB(16) NOT NULL,
    entity_type     TEXT NOT NULL,
    provider        TEXT NOT NULL,
    external_id     TEXT NOT NULL,
    last_synced_at  TEXT NOT NULL,
    sync_direction  TEXT NOT NULL
                    DEFAULT 'import_only'
                    CHECK (sync_direction IN (
                           'bidirectional',
                           'import_only',
                           'export_only')),
    sync_hash       TEXT,
    PRIMARY KEY (entity_id, entity_type, provider)
);

-- -------------------------------------------
-- Full-text search (FTS5)
-- -------------------------------------------
CREATE VIRTUAL TABLE notes_fts USING fts5(
    title, content,
    content=notes, content_rowid=rowid
);

CREATE VIRTUAL TABLE refs_fts USING fts5(
    title, abstract_text, citekey,
    content=refs, content_rowid=rowid
);

-- Triggers: keep FTS in sync with source tables

CREATE TRIGGER notes_ai AFTER INSERT ON notes BEGIN
    INSERT INTO notes_fts(rowid, title, content)
    VALUES (new.rowid, new.title, new.content);
END;

CREATE TRIGGER notes_ad AFTER DELETE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, title, content)
    VALUES ('delete', old.rowid, old.title, old.content);
END;

CREATE TRIGGER notes_au AFTER UPDATE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, title, content)
    VALUES ('delete', old.rowid, old.title, old.content);
    INSERT INTO notes_fts(rowid, title, content)
    VALUES (new.rowid, new.title, new.content);
END;

CREATE TRIGGER refs_ai AFTER INSERT ON refs BEGIN
    INSERT INTO refs_fts(rowid, title,
                         abstract_text, citekey)
    VALUES (new.rowid, new.title,
            new.abstract_text, new.citekey);
END;

CREATE TRIGGER refs_ad AFTER DELETE ON refs BEGIN
    INSERT INTO refs_fts(refs_fts, rowid, title,
                         abstract_text, citekey)
    VALUES ('delete', old.rowid, old.title,
            old.abstract_text, old.citekey);
END;

CREATE TRIGGER refs_au AFTER UPDATE ON refs BEGIN
    INSERT INTO refs_fts(refs_fts, rowid, title,
                         abstract_text, citekey)
    VALUES ('delete', old.rowid, old.title,
            old.abstract_text, old.citekey);
    INSERT INTO refs_fts(rowid, title,
                         abstract_text, citekey)
    VALUES (new.rowid, new.title,
            new.abstract_text, new.citekey);
END;
