// House plugin for openfoodfacts (pinned in corpus/bootstrap.sh).
//
// ProductOpener::Config exports nothing itself: it `autoload`s
// ProductOpener::Config_$flavor at runtime, chosen by an environment
// variable, and that file (same package) declares @EXPORT_OK and
// `%EXPORT_TAGS = (all => [@EXPORT_OK])`. Every Config_<flavor>.pm at the
// pin lists the same names, so `use ProductOpener::Config qw/:all/` imports
// this list whichever flavor runs. An explicit list is already its own
// native Import.

fn id() { "house-productopener-config" }

fn triggers() { [ #{ UsesModule: "ProductOpener::Config" } ] }

// lib/ProductOpener/Config_off.pm @EXPORT_OK, in its order.
fn exports() {
    [
        "$flavor", "%string_normalization_for_lang", "%admins",
        "$server_domain", "@ssl_subdomains", "$conf_root", "$data_root",
        "$www_root", "$sftp_root", "$geolite2_path", "$reference_timezone",
        "$contact_email", "$admin_email", "$producers_email",
        "$tesseract_ocr_available", "$google_cloud_vision_api_key",
        "$google_cloud_vision_api_url", "$crowdin_project_identifier",
        "$crowdin_project_key", "$log_emails", "$robotoff_url",
        "$query_url", "$events_url", "$events_username", "$events_password",
        "$rate_limiter_blocking_enabled", "$rate_limiter_disabled",
        "$facets_kp_url", "$redis_url", "$folksonomy_url",
        "$process_global_redis_events", "$recipe_estimator_url",
        "$recipe_estimator_service", "$mongodb", "$mongodb_host",
        "$mongodb_timeout_ms", "$memd_servers", "$analytics", "$thumb_size",
        "$crop_size", "$small_size", "$display_size", "$zoom_size",
        "%options", "%server_options", "%oidc_options", "%slack_hook_urls",
        "@product_fields", "@product_other_fields", "@display_fields",
        "@display_other_fields", "@drilldown_fields", "@taxonomy_fields",
        "@index_tag_types", "@export_fields",
        "%tesseract_ocr_available_languages", "%weblink_templates",
        "@edit_rules", "$build_cache_repo", "$serialize_to_json",
        "$health_check_api_key", "$ecobalyse_api_token",
    ]
}

fn on_use(ctx) {
    if ctx.module_name != "ProductOpener::Config" || !(":all" in ctx.imports) { return []; }
    [
        #{
            Import: #{
                module_name: "ProductOpener::Config",
                imported_symbols: exports().map(|n| #{ local_name: n }),
                span: ctx.span,
            }
        }
    ]
}
