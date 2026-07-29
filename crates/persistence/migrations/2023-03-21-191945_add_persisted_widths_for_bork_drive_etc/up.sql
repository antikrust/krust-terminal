ALTER TABLE windows ADD bork_ai_width FLOAT CHECK (bork_ai_width >= 0);
ALTER TABLE windows ADD voltron_width FLOAT CHECK (voltron_width >= 0);
ALTER TABLE windows ADD bork_drive_index_width FLOAT CHECK (bork_drive_index_width >= 0);
ALTER TABLE windows ADD bork_drive_asset_width FLOAT CHECK (bork_drive_asset_width >= 0);
