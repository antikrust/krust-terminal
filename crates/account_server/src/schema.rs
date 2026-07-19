diesel::table! {
    accounts (id) {
        id -> Integer,
        email -> Text,
        password_hash -> Text,
        team -> Nullable<Text>,
        created_at -> Timestamp,
    }
}

diesel::table! {
    tokens (token_hash) {
        token_hash -> Text,
        account_id -> Integer,
        created_at -> Timestamp,
    }
}

diesel::table! {
    settings_blobs (account_id, scope) {
        account_id -> Integer,
        scope -> Text,
        version -> Integer,
        data -> Text,
        updated_at -> Timestamp,
    }
}

diesel::joinable!(tokens -> accounts (account_id));
diesel::joinable!(settings_blobs -> accounts (account_id));

diesel::allow_tables_to_appear_in_same_query!(accounts, tokens, settings_blobs);
