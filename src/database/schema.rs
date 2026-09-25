// @generated automatically by Diesel CLI.

diesel::table! {
    check_result (id) {
        id -> Integer,
        monitor_id -> Integer,
        monitor_type -> Text,
        status -> Integer,
        response_time -> Integer,
        metadata_json -> Nullable<Text>,
        created_at -> Nullable<Timestamp>,
        updated_at -> Nullable<Timestamp>,
    }
}

diesel::table! {
    monitor_config (id) {
        id -> Integer,
        name -> Nullable<Text>,
        target -> Text,
        method -> Nullable<Text>,
        monitor_type -> Text,
        interval_ms -> Nullable<Integer>,
        timeout_ms -> Integer,
        config_json -> Nullable<Text>,
        enabled -> Integer,
        tag -> Nullable<Text>,
        created_at -> Nullable<Timestamp>,
        updated_at -> Nullable<Timestamp>,
    }
}

diesel::table! {
    alert_state (monitor_id) {
        monitor_id -> Integer,
        alerting -> Integer,
        updated_at -> Nullable<Timestamp>,
    }
}

diesel::table! {
    alert_history (id) {
        id -> Integer,
        monitor_id -> Integer,
        alert_type -> Text,
        state -> Text,
        message -> Nullable<Text>,
        created_at -> Nullable<Timestamp>,
    }
}

diesel::joinable!(check_result -> monitor_config (monitor_id));
diesel::joinable!(alert_state -> monitor_config (monitor_id));
diesel::joinable!(alert_history -> monitor_config (monitor_id));

diesel::allow_tables_to_appear_in_same_query!(
    check_result,
    monitor_config,
    alert_state,
    alert_history,
);
