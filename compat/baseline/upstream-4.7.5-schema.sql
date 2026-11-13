--
-- PostgreSQL database dump
--


-- Dumped from database version 15.19
-- Dumped by pg_dump version 15.19

SET statement_timeout = 0;
SET lock_timeout = 0;
SET idle_in_transaction_session_timeout = 0;
SET client_encoding = 'UTF8';
SET standard_conforming_strings = on;
SELECT pg_catalog.set_config('search_path', '', false);
SET check_function_bodies = false;
SET xmloption = content;
SET client_min_messages = warning;
SET row_security = off;

SET default_tablespace = '';

SET default_table_access_method = heap;

--
-- Name: activity_log; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.activity_log (
    id bigint NOT NULL,
    log_name character varying(255),
    description text NOT NULL,
    subject_type character varying(255),
    subject_id bigint,
    causer_type character varying(255),
    causer_id bigint,
    properties jsonb,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    event character varying(255),
    batch_uuid uuid
);


--
-- Name: activity_log_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.activity_log_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: activity_log_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.activity_log_id_seq OWNED BY public.activity_log.id;


--
-- Name: additional_destinations; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.additional_destinations (
    id bigint NOT NULL,
    application_id bigint NOT NULL,
    server_id bigint NOT NULL,
    status character varying(255) DEFAULT 'exited'::character varying NOT NULL,
    standalone_docker_id bigint NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone
);


--
-- Name: additional_destinations_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.additional_destinations_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: additional_destinations_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.additional_destinations_id_seq OWNED BY public.additional_destinations.id;


--
-- Name: application_deployment_queues; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.application_deployment_queues (
    id bigint NOT NULL,
    application_id character varying(255) NOT NULL,
    deployment_uuid character varying(255) NOT NULL,
    pull_request_id integer DEFAULT 0 NOT NULL,
    force_rebuild boolean DEFAULT false NOT NULL,
    commit character varying(255) DEFAULT 'HEAD'::character varying NOT NULL,
    status character varying(255) DEFAULT 'queued'::character varying NOT NULL,
    is_webhook boolean DEFAULT false NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    logs text,
    current_process_id character varying(255),
    restart_only boolean DEFAULT false NOT NULL,
    git_type character varying(255),
    server_id integer,
    application_name character varying(255),
    server_name character varying(255),
    deployment_url character varying(255),
    destination_id character varying(255),
    only_this_server boolean DEFAULT false NOT NULL,
    rollback boolean DEFAULT false NOT NULL,
    commit_message text,
    is_api boolean DEFAULT false NOT NULL,
    build_server_id integer,
    horizon_job_id character varying(255),
    horizon_job_worker character varying(255),
    finished_at timestamp(0) without time zone,
    docker_registry_image_tag character varying(255),
    configuration_hash character varying(255),
    configuration_snapshot text,
    configuration_diff text,
    parent_deployment_uuid character varying(255),
    scan_gate_result jsonb,
    scan_gate_bypassed_by bigint,
    deploy_started_at timestamp(0) without time zone,
    is_mcp boolean DEFAULT false NOT NULL,
    github_deployment_id bigint,
    github_deployment_state character varying(255)
)
WITH (fillfactor='90');


--
-- Name: application_deployment_queues_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.application_deployment_queues_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: application_deployment_queues_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.application_deployment_queues_id_seq OWNED BY public.application_deployment_queues.id;


--
-- Name: application_previews; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.application_previews (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    pull_request_id integer NOT NULL,
    pull_request_html_url character varying(255) NOT NULL,
    pull_request_issue_comment_id character varying(255),
    fqdn text,
    status character varying(255) DEFAULT 'exited'::character varying NOT NULL,
    application_id bigint NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    git_type character varying(255),
    docker_compose_domains text,
    last_online_at timestamp(0) without time zone DEFAULT '2026-11-13 08:05:19'::timestamp without time zone NOT NULL,
    deleted_at timestamp(0) without time zone,
    docker_registry_image_tag character varying(255),
    domain_dns_statuses json,
    restart_count integer DEFAULT 0 NOT NULL,
    max_restart_count integer DEFAULT 0 NOT NULL,
    restart_limit_reached boolean DEFAULT false NOT NULL,
    last_restart_at timestamp(0) without time zone,
    last_restart_type character varying(10),
    domain_port_overrides json,
    keeps_production_values boolean DEFAULT false NOT NULL
);


--
-- Name: application_previews_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.application_previews_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: application_previews_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.application_previews_id_seq OWNED BY public.application_previews.id;


--
-- Name: application_settings; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.application_settings (
    id bigint NOT NULL,
    is_static boolean DEFAULT false NOT NULL,
    is_git_submodules_enabled boolean DEFAULT true NOT NULL,
    is_git_lfs_enabled boolean DEFAULT true NOT NULL,
    is_auto_deploy_enabled boolean DEFAULT true NOT NULL,
    is_force_https_enabled boolean DEFAULT true NOT NULL,
    is_debug_enabled boolean DEFAULT false NOT NULL,
    is_preview_deployments_enabled boolean DEFAULT false NOT NULL,
    application_id bigint NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    is_log_drain_enabled boolean DEFAULT false NOT NULL,
    is_gpu_enabled boolean DEFAULT false NOT NULL,
    gpu_driver character varying(255) DEFAULT 'nvidia'::character varying NOT NULL,
    gpu_count character varying(255),
    gpu_device_ids character varying(255),
    gpu_options text,
    is_include_timestamps boolean DEFAULT false NOT NULL,
    is_swarm_only_worker_nodes boolean DEFAULT true NOT NULL,
    is_raw_compose_deployment_enabled boolean DEFAULT false NOT NULL,
    is_build_server_enabled boolean DEFAULT false NOT NULL,
    is_consistent_container_name_enabled boolean DEFAULT false NOT NULL,
    is_gzip_enabled boolean DEFAULT true NOT NULL,
    is_stripprefix_enabled boolean DEFAULT true NOT NULL,
    connect_to_docker_network boolean DEFAULT false NOT NULL,
    custom_internal_name character varying(255),
    is_container_label_escape_enabled boolean DEFAULT true NOT NULL,
    is_env_sorting_enabled boolean DEFAULT false NOT NULL,
    is_container_label_readonly_enabled boolean DEFAULT true NOT NULL,
    is_preserve_repository_enabled boolean DEFAULT false NOT NULL,
    disable_build_cache boolean DEFAULT false NOT NULL,
    is_spa boolean DEFAULT false NOT NULL,
    is_git_shallow_clone_enabled boolean DEFAULT true NOT NULL,
    is_pr_deployments_public_enabled boolean DEFAULT false NOT NULL,
    use_build_secrets boolean DEFAULT false NOT NULL,
    stop_grace_period integer,
    inject_build_args_to_dockerfile boolean DEFAULT true NOT NULL,
    include_source_commit_in_build boolean DEFAULT false NOT NULL,
    docker_images_to_keep integer DEFAULT 2 NOT NULL,
    custom_container_name_prefix character varying(255),
    redact_all_env_values_in_logs boolean DEFAULT true NOT NULL,
    traefik_certificate_resolver character varying(255),
    include_coolify_default_variables boolean DEFAULT true NOT NULL,
    is_permanent_redirect_enabled boolean DEFAULT false NOT NULL,
    rolling_update_drain_seconds integer DEFAULT 5 NOT NULL,
    is_image_scan_enabled boolean DEFAULT true NOT NULL,
    image_builds_by_team boolean,
    is_auto_cancel_queued_deployments_enabled boolean DEFAULT true NOT NULL,
    is_auto_cancel_building_deployments_enabled boolean DEFAULT false NOT NULL,
    include_network_hosts_in_build boolean DEFAULT true NOT NULL
);


--
-- Name: COLUMN application_settings.stop_grace_period; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.application_settings.stop_grace_period IS 'Seconds to wait for graceful shutdown before forcing container stop (1-3600). Null uses default of 30 seconds.';


--
-- Name: COLUMN application_settings.rolling_update_drain_seconds; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.application_settings.rolling_update_drain_seconds IS 'Seconds to wait after the new container is healthy before old containers stop during a rolling update (0-300). 0 turns the wait off.';


--
-- Name: application_settings_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.application_settings_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: application_settings_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.application_settings_id_seq OWNED BY public.application_settings.id;


--
-- Name: applications; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.applications (
    id bigint NOT NULL,
    repository_project_id integer,
    uuid character varying(255) NOT NULL,
    name character varying(255) NOT NULL,
    fqdn text,
    config_hash character varying(255),
    git_repository character varying(255) NOT NULL,
    git_branch character varying(255) NOT NULL,
    git_commit_sha character varying(255) DEFAULT 'HEAD'::character varying NOT NULL,
    git_full_url character varying(255),
    docker_registry_image_name character varying(255),
    docker_registry_image_tag character varying(255),
    build_pack character varying(255) NOT NULL,
    static_image character varying(255) DEFAULT 'nginx:alpine'::character varying NOT NULL,
    install_command character varying(255),
    build_command character varying(255),
    start_command character varying(255),
    ports_exposes character varying(255),
    ports_mappings character varying(255),
    base_directory character varying(255) DEFAULT '/'::character varying NOT NULL,
    publish_directory character varying(255),
    health_check_path character varying(255) DEFAULT '/'::character varying NOT NULL,
    health_check_port character varying(255),
    health_check_host character varying(255) DEFAULT 'localhost'::character varying NOT NULL,
    health_check_method character varying(255) DEFAULT 'GET'::character varying NOT NULL,
    health_check_return_code integer DEFAULT 200 NOT NULL,
    health_check_scheme character varying(255) DEFAULT 'http'::character varying NOT NULL,
    health_check_response_text character varying(255),
    health_check_interval integer DEFAULT 5 NOT NULL,
    health_check_timeout integer DEFAULT 5 NOT NULL,
    health_check_retries integer DEFAULT 10 NOT NULL,
    health_check_start_period integer DEFAULT 5 NOT NULL,
    limits_memory character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_memory_swap character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_memory_swappiness integer DEFAULT 60 NOT NULL,
    limits_memory_reservation character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_cpus character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_cpuset character varying(255),
    limits_cpu_shares integer DEFAULT 1024 NOT NULL,
    status character varying(255) DEFAULT 'exited'::character varying NOT NULL,
    preview_url_template character varying(255) DEFAULT '{{pr_id}}.{{domain}}'::character varying NOT NULL,
    destination_type character varying(255),
    destination_id bigint,
    source_type character varying(255),
    source_id bigint,
    private_key_id bigint,
    environment_id bigint NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    description character varying(255),
    dockerfile text,
    health_check_enabled boolean DEFAULT false NOT NULL,
    dockerfile_location character varying(255),
    custom_labels text,
    dockerfile_target_build character varying(255),
    manual_webhook_secret_github text,
    manual_webhook_secret_gitlab text,
    docker_compose_location character varying(255) DEFAULT '/docker-compose.yaml'::character varying,
    docker_compose text,
    docker_compose_raw text,
    docker_compose_domains text,
    deleted_at timestamp(0) without time zone,
    docker_compose_custom_start_command character varying(255),
    docker_compose_custom_build_command character varying(255),
    swarm_replicas integer DEFAULT 1 NOT NULL,
    swarm_placement_constraints text,
    manual_webhook_secret_bitbucket text,
    custom_docker_run_options text,
    post_deployment_command text,
    post_deployment_command_container character varying(255),
    pre_deployment_command text,
    pre_deployment_command_container character varying(255),
    watch_paths text,
    custom_healthcheck_found boolean DEFAULT false NOT NULL,
    manual_webhook_secret_gitea text,
    redirect character varying(255) DEFAULT 'both'::character varying NOT NULL,
    compose_parsing_version character varying(255) DEFAULT '1'::character varying NOT NULL,
    last_online_at timestamp(0) without time zone DEFAULT '2026-11-13 08:05:19'::timestamp without time zone NOT NULL,
    custom_nginx_configuration text,
    custom_network_aliases text,
    is_http_basic_auth_enabled boolean DEFAULT false NOT NULL,
    http_basic_auth_username character varying(255),
    http_basic_auth_password character varying(255),
    restart_count integer DEFAULT 0 NOT NULL,
    last_restart_at timestamp(0) without time zone,
    last_restart_type character varying(10),
    health_check_type text DEFAULT 'http'::text NOT NULL,
    health_check_command text,
    max_restart_count integer DEFAULT 0 NOT NULL,
    noindex_domains json,
    domain_dns_statuses json,
    container_present boolean,
    restart_limit_reached boolean DEFAULT false NOT NULL,
    domain_port_overrides json,
    is_maintenance_enabled boolean DEFAULT false NOT NULL,
    maintenance_page text,
    http_basic_auth_hash_algorithm character varying(255) DEFAULT 'bcrypt'::character varying NOT NULL,
    http_basic_auth_bcrypt_cost smallint DEFAULT '10'::smallint NOT NULL,
    http_basic_auth_argon2id_memory_cost integer DEFAULT 65536 NOT NULL,
    http_basic_auth_argon2id_time_cost smallint DEFAULT '4'::smallint NOT NULL,
    git_https_username character varying(255),
    git_https_password text,
    docker_compose_image_overrides json,
    git_ssh_known_hosts text,
    CONSTRAINT applications_redirect_check CHECK (((redirect)::text = ANY ((ARRAY['www'::character varying, 'non-www'::character varying, 'both'::character varying])::text[])))
)
WITH (fillfactor='70', autovacuum_vacuum_scale_factor='0.05');


--
-- Name: applications_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.applications_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: applications_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.applications_id_seq OWNED BY public.applications.id;


--
-- Name: audit_events; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.audit_events (
    id bigint NOT NULL,
    team_id bigint,
    event character varying(255) NOT NULL,
    source character varying(32) NOT NULL,
    action character varying(64) NOT NULL,
    actor_type character varying(32) NOT NULL,
    actor_id bigint,
    actor_name character varying(255),
    actor_email character varying(255),
    actor_token_id bigint,
    actor_token_name character varying(255),
    resource_type character varying(255),
    resource_uuid character varying(255),
    resource_name character varying(255),
    description text NOT NULL,
    metadata json,
    ip_address character varying(45),
    user_agent character varying(200),
    created_at timestamp(0) without time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
    changes text,
    level character varying(16) DEFAULT 'info'::character varying NOT NULL
);


--
-- Name: audit_events_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.audit_events_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: audit_events_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.audit_events_id_seq OWNED BY public.audit_events.id;


--
-- Name: cloud_init_scripts; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.cloud_init_scripts (
    id bigint NOT NULL,
    team_id bigint NOT NULL,
    name character varying(255) NOT NULL,
    script text NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    uuid character varying(255)
);


--
-- Name: cloud_init_scripts_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.cloud_init_scripts_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: cloud_init_scripts_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.cloud_init_scripts_id_seq OWNED BY public.cloud_init_scripts.id;


--
-- Name: cloud_provider_tokens; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.cloud_provider_tokens (
    id bigint NOT NULL,
    team_id bigint NOT NULL,
    provider character varying(255) NOT NULL,
    token text NOT NULL,
    name character varying(255),
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    uuid character varying(255) NOT NULL,
    description text
);


--
-- Name: cloud_provider_tokens_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.cloud_provider_tokens_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: cloud_provider_tokens_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.cloud_provider_tokens_id_seq OWNED BY public.cloud_provider_tokens.id;


--
-- Name: database_backup_s3_replicas; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.database_backup_s3_replicas (
    id bigint NOT NULL,
    execution_id bigint NOT NULL,
    s3_storage_id bigint,
    s3_uploaded boolean,
    s3_storage_deleted boolean DEFAULT false NOT NULL,
    message text,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone
);


--
-- Name: database_backup_s3_replicas_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.database_backup_s3_replicas_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: database_backup_s3_replicas_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.database_backup_s3_replicas_id_seq OWNED BY public.database_backup_s3_replicas.id;


--
-- Name: discord_notification_settings; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.discord_notification_settings (
    id bigint NOT NULL,
    team_id bigint NOT NULL,
    discord_enabled boolean DEFAULT false NOT NULL,
    discord_webhook_url text,
    deployment_success_discord_notifications boolean DEFAULT false NOT NULL,
    deployment_failure_discord_notifications boolean DEFAULT true NOT NULL,
    status_change_discord_notifications boolean DEFAULT false NOT NULL,
    backup_success_discord_notifications boolean DEFAULT false NOT NULL,
    backup_failure_discord_notifications boolean DEFAULT true NOT NULL,
    scheduled_task_success_discord_notifications boolean DEFAULT false NOT NULL,
    scheduled_task_failure_discord_notifications boolean DEFAULT true NOT NULL,
    docker_cleanup_success_discord_notifications boolean DEFAULT false NOT NULL,
    docker_cleanup_failure_discord_notifications boolean DEFAULT true NOT NULL,
    server_disk_usage_discord_notifications boolean DEFAULT true NOT NULL,
    server_reachable_discord_notifications boolean DEFAULT false NOT NULL,
    server_unreachable_discord_notifications boolean DEFAULT true NOT NULL,
    discord_ping_enabled boolean DEFAULT true NOT NULL,
    server_patch_discord_notifications boolean DEFAULT true NOT NULL,
    traefik_outdated_discord_notifications boolean DEFAULT true NOT NULL,
    restart_limit_reached_discord_notifications boolean DEFAULT true NOT NULL,
    failure_count integer DEFAULT 0 NOT NULL,
    image_vulnerabilities_discord_notifications boolean DEFAULT false NOT NULL
);


--
-- Name: discord_notification_settings_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.discord_notification_settings_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: discord_notification_settings_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.discord_notification_settings_id_seq OWNED BY public.discord_notification_settings.id;


--
-- Name: dns_provider_zones; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.dns_provider_zones (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    integration_token_id bigint NOT NULL,
    provider_zone_id character varying(255) NOT NULL,
    name character varying(255) NOT NULL,
    account_id character varying(255),
    account_name character varying(255),
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone
);


--
-- Name: dns_provider_zones_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.dns_provider_zones_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: dns_provider_zones_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.dns_provider_zones_id_seq OWNED BY public.dns_provider_zones.id;


--
-- Name: docker_cleanup_executions; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.docker_cleanup_executions (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    status character varying(255) DEFAULT 'running'::character varying NOT NULL,
    message text,
    cleanup_log json,
    server_id bigint NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    finished_at timestamp(0) without time zone,
    CONSTRAINT docker_cleanup_executions_status_check CHECK (((status)::text = ANY ((ARRAY['success'::character varying, 'failed'::character varying, 'running'::character varying])::text[])))
);


--
-- Name: docker_cleanup_executions_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.docker_cleanup_executions_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: docker_cleanup_executions_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.docker_cleanup_executions_id_seq OWNED BY public.docker_cleanup_executions.id;


--
-- Name: email_notification_settings; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.email_notification_settings (
    id bigint NOT NULL,
    team_id bigint NOT NULL,
    smtp_enabled boolean DEFAULT false NOT NULL,
    smtp_from_address text,
    smtp_from_name text,
    smtp_recipients text,
    smtp_host text,
    smtp_port integer,
    smtp_encryption character varying(255),
    smtp_username text,
    smtp_password text,
    smtp_timeout integer,
    resend_enabled boolean DEFAULT false NOT NULL,
    resend_api_key text,
    use_instance_email_settings boolean DEFAULT false NOT NULL,
    deployment_success_email_notifications boolean DEFAULT false NOT NULL,
    deployment_failure_email_notifications boolean DEFAULT true NOT NULL,
    status_change_email_notifications boolean DEFAULT false NOT NULL,
    backup_success_email_notifications boolean DEFAULT false NOT NULL,
    backup_failure_email_notifications boolean DEFAULT true NOT NULL,
    scheduled_task_success_email_notifications boolean DEFAULT false NOT NULL,
    scheduled_task_failure_email_notifications boolean DEFAULT true NOT NULL,
    docker_cleanup_success_email_notifications boolean DEFAULT false NOT NULL,
    docker_cleanup_failure_email_notifications boolean DEFAULT true NOT NULL,
    server_disk_usage_email_notifications boolean DEFAULT true NOT NULL,
    server_reachable_email_notifications boolean DEFAULT false NOT NULL,
    server_unreachable_email_notifications boolean DEFAULT true NOT NULL,
    server_patch_email_notifications boolean DEFAULT true NOT NULL,
    traefik_outdated_email_notifications boolean DEFAULT true NOT NULL,
    smtp_ehlo_domain character varying(255),
    restart_limit_reached_email_notifications boolean DEFAULT true NOT NULL,
    failure_count integer DEFAULT 0 NOT NULL,
    deployment_commit_details_email_notifications boolean DEFAULT false NOT NULL,
    image_vulnerabilities_email_notifications boolean DEFAULT true NOT NULL,
    smtp_server_name character varying(255),
    smtp_servers text,
    resend_from_address text,
    resend_from_name text
);


--
-- Name: email_notification_settings_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.email_notification_settings_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: email_notification_settings_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.email_notification_settings_id_seq OWNED BY public.email_notification_settings.id;


--
-- Name: environment_variables; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.environment_variables (
    id bigint NOT NULL,
    key character varying(255) NOT NULL,
    value text,
    is_preview boolean DEFAULT false NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    is_shown_once boolean DEFAULT false NOT NULL,
    is_multiline boolean DEFAULT false NOT NULL,
    version character varying(255) DEFAULT '4.0.0-beta.239'::character varying NOT NULL,
    is_literal boolean DEFAULT false NOT NULL,
    uuid character varying(255) NOT NULL,
    "order" integer,
    is_required boolean DEFAULT false NOT NULL,
    is_shared boolean DEFAULT false NOT NULL,
    resourceable_type character varying(255),
    resourceable_id bigint,
    is_runtime boolean DEFAULT true NOT NULL,
    is_buildtime boolean DEFAULT true NOT NULL,
    comment character varying(256),
    uses_legacy_escaping boolean DEFAULT false NOT NULL,
    preserve_compose_null boolean DEFAULT false NOT NULL
);


--
-- Name: environment_variables_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.environment_variables_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: environment_variables_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.environment_variables_id_seq OWNED BY public.environment_variables.id;


--
-- Name: environments; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.environments (
    id bigint NOT NULL,
    name character varying(255) NOT NULL,
    project_id bigint NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    description character varying(255),
    uuid character varying(255) NOT NULL,
    canvas_positions json
);


--
-- Name: environments_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.environments_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: environments_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.environments_id_seq OWNED BY public.environments.id;


--
-- Name: failed_jobs; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.failed_jobs (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    connection text NOT NULL,
    queue text NOT NULL,
    payload text NOT NULL,
    exception text NOT NULL,
    failed_at timestamp(0) without time zone DEFAULT CURRENT_TIMESTAMP NOT NULL
);


--
-- Name: failed_jobs_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.failed_jobs_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: failed_jobs_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.failed_jobs_id_seq OWNED BY public.failed_jobs.id;


--
-- Name: gitea_apps; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.gitea_apps (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    name character varying(255) NOT NULL,
    organization character varying(255),
    api_url character varying(255) NOT NULL,
    html_url character varying(255) NOT NULL,
    custom_port integer DEFAULT 22 NOT NULL,
    custom_user character varying(255) DEFAULT 'git'::character varying NOT NULL,
    is_system_wide boolean DEFAULT false NOT NULL,
    is_public boolean DEFAULT false NOT NULL,
    client_id text,
    client_secret text,
    access_token text,
    refresh_token text,
    expires_at integer,
    redirect_uri character varying(255),
    webhook_secret text,
    private_key_id bigint,
    team_id bigint NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone
);


--
-- Name: gitea_apps_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.gitea_apps_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: gitea_apps_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.gitea_apps_id_seq OWNED BY public.gitea_apps.id;


--
-- Name: github_apps; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.github_apps (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    name character varying(255) NOT NULL,
    organization character varying(255),
    api_url character varying(255) NOT NULL,
    html_url character varying(255) NOT NULL,
    custom_user character varying(255) DEFAULT 'git'::character varying NOT NULL,
    custom_port integer DEFAULT 22 NOT NULL,
    app_id integer,
    installation_id integer,
    client_id character varying(255),
    client_secret text,
    webhook_secret text,
    is_system_wide boolean DEFAULT false NOT NULL,
    is_public boolean DEFAULT false NOT NULL,
    private_key_id bigint,
    team_id bigint NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    contents character varying(255),
    metadata character varying(255),
    pull_requests character varying(255),
    administration character varying(255),
    organization_self_hosted_runners character varying(255),
    actions character varying(255),
    webhook_events json,
    runner_group_id bigint,
    deployments character varying(255)
);


--
-- Name: github_apps_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.github_apps_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: github_apps_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.github_apps_id_seq OWNED BY public.github_apps.id;


--
-- Name: github_runner_configs; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.github_runner_configs (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    server_id bigint NOT NULL,
    github_app_id bigint NOT NULL,
    labels json NOT NULL,
    is_enabled boolean DEFAULT true NOT NULL,
    is_dedicated boolean DEFAULT false NOT NULL,
    max_runners smallint DEFAULT '2'::smallint NOT NULL,
    docker_mode character varying(255) DEFAULT 'none'::character varying NOT NULL,
    runner_image character varying(255),
    cpu_limit character varying(255),
    memory_limit character varying(255),
    capacity_wait_timeout integer DEFAULT 60 NOT NULL,
    idle_timeout integer DEFAULT 10 NOT NULL,
    job_timeout integer DEFAULT 360 NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    allow_pull_requests boolean DEFAULT false NOT NULL
);


--
-- Name: github_runner_configs_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.github_runner_configs_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: github_runner_configs_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.github_runner_configs_id_seq OWNED BY public.github_runner_configs.id;


--
-- Name: github_runner_executions; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.github_runner_executions (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    github_app_id bigint NOT NULL,
    github_runner_config_id bigint,
    server_id bigint,
    status character varying(255) DEFAULT 'queued'::character varying NOT NULL,
    trigger_workflow_job_id bigint NOT NULL,
    workflow_job_id bigint,
    workflow_job_html_url character varying(255),
    workflow_name character varying(255),
    job_name character varying(255),
    repository_full_name character varying(255),
    labels json,
    runner_name character varying(255),
    runner_id bigint,
    conclusion character varying(255),
    error_message text,
    provision_attempts smallint DEFAULT '0'::smallint NOT NULL,
    queued_at timestamp(0) without time zone,
    ready_at timestamp(0) without time zone,
    started_at timestamp(0) without time zone,
    completed_at timestamp(0) without time zone,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    is_pull_request boolean DEFAULT false NOT NULL
);


--
-- Name: github_runner_executions_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.github_runner_executions_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: github_runner_executions_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.github_runner_executions_id_seq OWNED BY public.github_runner_executions.id;


--
-- Name: gitlab_apps; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.gitlab_apps (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    name character varying(255) NOT NULL,
    organization character varying(255),
    api_url character varying(255) NOT NULL,
    html_url character varying(255) NOT NULL,
    custom_port integer DEFAULT 22 NOT NULL,
    custom_user character varying(255) DEFAULT 'git'::character varying NOT NULL,
    is_system_wide boolean DEFAULT false NOT NULL,
    is_public boolean DEFAULT false NOT NULL,
    app_id integer,
    app_secret text,
    oauth_id integer,
    group_name character varying(255),
    public_key text,
    webhook_token text,
    deploy_key_id integer,
    private_key_id bigint,
    team_id bigint NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    client_id text,
    client_secret text,
    access_token text,
    refresh_token text,
    expires_at integer,
    redirect_uri character varying(255)
);


--
-- Name: gitlab_apps_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.gitlab_apps_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: gitlab_apps_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.gitlab_apps_id_seq OWNED BY public.gitlab_apps.id;


--
-- Name: image_sboms; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.image_sboms (
    id bigint NOT NULL,
    server_id bigint NOT NULL,
    image_id character varying(80) NOT NULL,
    platform character varying(32) NOT NULL,
    packages jsonb NOT NULL,
    packages_truncated boolean DEFAULT false NOT NULL,
    last_used_at timestamp(0) without time zone NOT NULL
);


--
-- Name: image_sboms_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.image_sboms_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: image_sboms_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.image_sboms_id_seq OWNED BY public.image_sboms.id;


--
-- Name: image_scan_ignores; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.image_scan_ignores (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    team_id bigint NOT NULL,
    resource_type character varying(255),
    resource_id bigint,
    vulnerability_id character varying(64),
    package character varying(214),
    reason character varying(500) NOT NULL,
    expires_at timestamp(0) without time zone,
    created_by bigint,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone
);


--
-- Name: image_scan_ignores_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.image_scan_ignores_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: image_scan_ignores_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.image_scan_ignores_id_seq OWNED BY public.image_scan_ignores.id;


--
-- Name: image_scan_notification_states; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.image_scan_notification_states (
    id bigint NOT NULL,
    resource_type character varying(255) NOT NULL,
    resource_id bigint NOT NULL,
    team_id bigint NOT NULL,
    notified_ids jsonb,
    notified_candidates jsonb,
    digest_ids jsonb,
    is_baseline boolean DEFAULT true NOT NULL,
    check_reported boolean DEFAULT false NOT NULL,
    held_digests smallint DEFAULT '0'::smallint NOT NULL,
    baseline_deployment_queue_id bigint,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone
);


--
-- Name: image_scan_notification_states_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.image_scan_notification_states_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: image_scan_notification_states_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.image_scan_notification_states_id_seq OWNED BY public.image_scan_notification_states.id;


--
-- Name: image_scan_policies; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.image_scan_policies (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    scope_type character varying(255),
    scope_id bigint,
    gate_mode character varying(255),
    on_scan_error character varying(255),
    count_medium boolean,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone
);


--
-- Name: image_scan_policies_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.image_scan_policies_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: image_scan_policies_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.image_scan_policies_id_seq OWNED BY public.image_scan_policies.id;


--
-- Name: image_scans; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.image_scans (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    server_id bigint NOT NULL,
    image_sbom_id bigint,
    image_id character varying(80) NOT NULL,
    repo_digest character varying(80),
    resource_type character varying(255) NOT NULL,
    resource_id bigint NOT NULL,
    image_ref character varying(512) NOT NULL,
    deployment_queue_id bigint,
    status character varying(255) NOT NULL,
    error_code character varying(255),
    error_context jsonb,
    summary jsonb,
    findings jsonb,
    candidates jsonb,
    base_image_ref character varying(512),
    builds_image boolean DEFAULT false NOT NULL,
    is_baseline boolean DEFAULT true NOT NULL,
    grype_db_built_at timestamp(0) without time zone,
    scanned_at timestamp(0) without time zone,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone
);


--
-- Name: image_scans_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.image_scans_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: image_scans_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.image_scans_id_seq OWNED BY public.image_scans.id;


--
-- Name: instance_settings; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.instance_settings (
    id bigint NOT NULL,
    public_ipv4 character varying(255),
    public_ipv6 character varying(255),
    fqdn character varying(255),
    public_port_min integer DEFAULT 9000 NOT NULL,
    public_port_max integer DEFAULT 9100 NOT NULL,
    do_not_track boolean DEFAULT false NOT NULL,
    is_auto_update_enabled boolean DEFAULT true NOT NULL,
    is_registration_enabled boolean DEFAULT true NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    next_channel boolean DEFAULT false NOT NULL,
    smtp_enabled boolean DEFAULT false NOT NULL,
    smtp_from_address text,
    smtp_from_name text,
    smtp_recipients text,
    smtp_host text,
    smtp_port integer,
    smtp_encryption character varying(255),
    smtp_username text,
    smtp_password text,
    smtp_timeout integer,
    resend_enabled boolean DEFAULT false NOT NULL,
    resend_api_key text,
    is_dns_validation_enabled boolean DEFAULT true NOT NULL,
    custom_dns_servers character varying(255) DEFAULT '1.1.1.1'::character varying,
    instance_name character varying(255),
    is_api_enabled boolean DEFAULT false NOT NULL,
    allowed_ips text,
    auto_update_frequency character varying(255) DEFAULT '0 0 * * *'::character varying NOT NULL,
    update_check_frequency character varying(255) DEFAULT '0 * * * *'::character varying NOT NULL,
    new_version_available boolean DEFAULT false NOT NULL,
    instance_timezone character varying(255) DEFAULT 'UTC'::character varying NOT NULL,
    helper_version character varying(255) DEFAULT '1.0.0'::character varying NOT NULL,
    disable_two_step_confirmation boolean DEFAULT false NOT NULL,
    is_sponsorship_popup_enabled boolean DEFAULT true NOT NULL,
    dev_helper_version character varying(255),
    is_wire_navigate_enabled boolean DEFAULT true NOT NULL,
    docker_registry_url character varying(255) DEFAULT 'docker.io'::character varying NOT NULL,
    is_mcp_server_enabled boolean DEFAULT false NOT NULL,
    disable_registration_when_oauth_enabled boolean DEFAULT false NOT NULL,
    webhook_allowed_internal_hosts json,
    webhook_allow_localhost boolean DEFAULT false NOT NULL,
    domain_connect_private_key text,
    avatar_storage_type character varying(255) DEFAULT 'local'::character varying NOT NULL,
    avatar_s3_storage_id bigint,
    is_dashboard_force_https_enabled boolean DEFAULT true NOT NULL,
    smtp_ehlo_domain character varying(255),
    image_cdn_url character varying(255),
    is_backup_before_update_enabled boolean DEFAULT true NOT NULL,
    is_passkey_enabled boolean DEFAULT true NOT NULL,
    file_download_s3_storage_id bigint,
    server_validation_rate_limit_seconds integer DEFAULT 600 NOT NULL,
    server_validation_max_failures_per_ip integer DEFAULT 10 NOT NULL,
    server_validation_max_failures_per_team integer DEFAULT 30 NOT NULL,
    update_channel character varying(255) DEFAULT 'stable'::character varying NOT NULL,
    update_custom_tag character varying(255),
    previous_update_channel character varying(255),
    notification_from_address text,
    notification_from_name text,
    smtp_server_name character varying(255),
    smtp_servers text,
    auto_update_target_version character varying(255),
    auto_update_started_at timestamp(0) without time zone,
    resend_from_address text,
    resend_from_name text
);


--
-- Name: instance_settings_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.instance_settings_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: instance_settings_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.instance_settings_id_seq OWNED BY public.instance_settings.id;


--
-- Name: integration_tokens; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.integration_tokens (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    team_id bigint NOT NULL,
    provider character varying(255) NOT NULL,
    name character varying(255) NOT NULL,
    token text NOT NULL,
    capabilities json NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    metadata json
);


--
-- Name: integration_tokens_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.integration_tokens_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: integration_tokens_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.integration_tokens_id_seq OWNED BY public.integration_tokens.id;


--
-- Name: local_file_volumes; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.local_file_volumes (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    fs_path text NOT NULL,
    mount_path text,
    content text,
    resource_type character varying(255),
    resource_id bigint,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    is_directory boolean DEFAULT false NOT NULL,
    chown character varying(255),
    chmod character varying(255),
    is_based_on_git boolean DEFAULT false NOT NULL,
    is_preview_suffix_enabled boolean DEFAULT true NOT NULL,
    is_host_file boolean DEFAULT false NOT NULL
);


--
-- Name: local_file_volumes_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.local_file_volumes_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: local_file_volumes_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.local_file_volumes_id_seq OWNED BY public.local_file_volumes.id;


--
-- Name: local_persistent_volumes; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.local_persistent_volumes (
    id bigint NOT NULL,
    name character varying(255) NOT NULL,
    mount_path character varying(255) NOT NULL,
    host_path character varying(255),
    container_id character varying(255),
    resource_type character varying(255),
    resource_id bigint,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    is_preview_suffix_enabled boolean DEFAULT true NOT NULL,
    uuid character varying(255) NOT NULL,
    standalone_sqlite_id bigint,
    ignores_compose_driver_options boolean DEFAULT false NOT NULL
);


--
-- Name: local_persistent_volumes_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.local_persistent_volumes_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: local_persistent_volumes_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.local_persistent_volumes_id_seq OWNED BY public.local_persistent_volumes.id;


--
-- Name: managed_dns_record_references; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.managed_dns_record_references (
    id bigint NOT NULL,
    managed_dns_record_id bigint NOT NULL,
    resource_type character varying(255) NOT NULL,
    resource_id bigint NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone
);


--
-- Name: managed_dns_record_references_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.managed_dns_record_references_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: managed_dns_record_references_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.managed_dns_record_references_id_seq OWNED BY public.managed_dns_record_references.id;


--
-- Name: managed_dns_records; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.managed_dns_records (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    team_id bigint NOT NULL,
    integration_token_id bigint NOT NULL,
    dns_provider_zone_id bigint NOT NULL,
    provider_record_id character varying(255) NOT NULL,
    type character varying(16) NOT NULL,
    name character varying(255) NOT NULL,
    content character varying(255) NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    owned boolean DEFAULT false NOT NULL
);


--
-- Name: managed_dns_records_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.managed_dns_records_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: managed_dns_records_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.managed_dns_records_id_seq OWNED BY public.managed_dns_records.id;


--
-- Name: migrations; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.migrations (
    id integer NOT NULL,
    migration character varying(255) NOT NULL,
    batch integer NOT NULL
);


--
-- Name: migrations_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.migrations_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: migrations_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.migrations_id_seq OWNED BY public.migrations.id;


--
-- Name: notification_throttles; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.notification_throttles (
    id bigint NOT NULL,
    notifiable_type character varying(255) NOT NULL,
    notifiable_id bigint NOT NULL,
    notification character varying(255) NOT NULL,
    sent_at timestamp(0) with time zone NOT NULL
);


--
-- Name: notification_throttles_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.notification_throttles_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: notification_throttles_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.notification_throttles_id_seq OWNED BY public.notification_throttles.id;


--
-- Name: oauth_identities; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.oauth_identities (
    id bigint NOT NULL,
    user_id bigint NOT NULL,
    provider character varying(255) NOT NULL,
    issuer character varying(255) NOT NULL,
    provider_user_id character varying(255) NOT NULL,
    email character varying(255),
    raw_claims json,
    last_login_at timestamp(0) without time zone,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone
);


--
-- Name: oauth_identities_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.oauth_identities_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: oauth_identities_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.oauth_identities_id_seq OWNED BY public.oauth_identities.id;


--
-- Name: oauth_settings; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.oauth_settings (
    id bigint NOT NULL,
    provider character varying(255) NOT NULL,
    enabled boolean DEFAULT false NOT NULL,
    client_id character varying(255),
    client_secret text,
    redirect_uri character varying(255),
    tenant character varying(255),
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    base_url character varying(255),
    custom_label character varying(255),
    scopes character varying(255),
    allow_registration boolean DEFAULT true NOT NULL,
    require_email_verified boolean DEFAULT true NOT NULL,
    use_pkce boolean DEFAULT true NOT NULL,
    clock_skew_seconds smallint DEFAULT '60'::smallint NOT NULL,
    auto_join_root_team boolean DEFAULT false NOT NULL
);


--
-- Name: oauth_settings_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.oauth_settings_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: oauth_settings_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.oauth_settings_id_seq OWNED BY public.oauth_settings.id;


--
-- Name: passkeys; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.passkeys (
    id bigint NOT NULL,
    user_id bigint NOT NULL,
    name character varying(255) NOT NULL,
    credential_id character varying(255) NOT NULL,
    credential json NOT NULL,
    last_used_at timestamp(0) without time zone,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone
);


--
-- Name: passkeys_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.passkeys_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: passkeys_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.passkeys_id_seq OWNED BY public.passkeys.id;


--
-- Name: password_reset_tokens; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.password_reset_tokens (
    email character varying(255) NOT NULL,
    token character varying(255) NOT NULL,
    created_at timestamp(0) without time zone
);


--
-- Name: personal_access_tokens; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.personal_access_tokens (
    id bigint NOT NULL,
    tokenable_type character varying(255) NOT NULL,
    tokenable_id bigint NOT NULL,
    name character varying(255) NOT NULL,
    token character varying(64) NOT NULL,
    team_id character varying(255) NOT NULL,
    abilities text,
    last_used_at timestamp(0) without time zone,
    expires_at timestamp(0) without time zone,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    api_token_expiration_warning_sent_at timestamp(0) without time zone,
    scopes json,
    paused_at timestamp(0) without time zone
);


--
-- Name: personal_access_tokens_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.personal_access_tokens_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: personal_access_tokens_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.personal_access_tokens_id_seq OWNED BY public.personal_access_tokens.id;


--
-- Name: private_keys; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.private_keys (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    name character varying(255) NOT NULL,
    description character varying(255),
    private_key text NOT NULL,
    is_git_related boolean DEFAULT false NOT NULL,
    team_id bigint NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    fingerprint character varying(255)
);


--
-- Name: private_keys_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.private_keys_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: private_keys_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.private_keys_id_seq OWNED BY public.private_keys.id;


--
-- Name: project_settings; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.project_settings (
    id bigint NOT NULL,
    project_id bigint NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone
);


--
-- Name: project_settings_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.project_settings_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: project_settings_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.project_settings_id_seq OWNED BY public.project_settings.id;


--
-- Name: projects; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.projects (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    name character varying(255) NOT NULL,
    description character varying(255),
    team_id bigint NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    icon_path character varying(255),
    icon_storage_type character varying(255),
    icon_s3_storage_id bigint,
    canvas_position json
);


--
-- Name: projects_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.projects_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: projects_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.projects_id_seq OWNED BY public.projects.id;


--
-- Name: pushover_notification_settings; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.pushover_notification_settings (
    id bigint NOT NULL,
    team_id bigint NOT NULL,
    pushover_enabled boolean DEFAULT false NOT NULL,
    pushover_user_key text,
    pushover_api_token text,
    deployment_success_pushover_notifications boolean DEFAULT false NOT NULL,
    deployment_failure_pushover_notifications boolean DEFAULT true NOT NULL,
    status_change_pushover_notifications boolean DEFAULT false NOT NULL,
    backup_success_pushover_notifications boolean DEFAULT false NOT NULL,
    backup_failure_pushover_notifications boolean DEFAULT true NOT NULL,
    scheduled_task_success_pushover_notifications boolean DEFAULT false NOT NULL,
    scheduled_task_failure_pushover_notifications boolean DEFAULT true NOT NULL,
    docker_cleanup_success_pushover_notifications boolean DEFAULT false NOT NULL,
    docker_cleanup_failure_pushover_notifications boolean DEFAULT true NOT NULL,
    server_disk_usage_pushover_notifications boolean DEFAULT true NOT NULL,
    server_reachable_pushover_notifications boolean DEFAULT false NOT NULL,
    server_unreachable_pushover_notifications boolean DEFAULT true NOT NULL,
    server_patch_pushover_notifications boolean DEFAULT true NOT NULL,
    traefik_outdated_pushover_notifications boolean DEFAULT true NOT NULL,
    restart_limit_reached_pushover_notifications boolean DEFAULT true NOT NULL,
    failure_count integer DEFAULT 0 NOT NULL,
    image_vulnerabilities_pushover_notifications boolean DEFAULT false NOT NULL
);


--
-- Name: pushover_notification_settings_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.pushover_notification_settings_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: pushover_notification_settings_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.pushover_notification_settings_id_seq OWNED BY public.pushover_notification_settings.id;


--
-- Name: resource_links; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.resource_links (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    environment_id bigint NOT NULL,
    source_type character varying(255) NOT NULL,
    source_id bigint NOT NULL,
    target_type character varying(255) NOT NULL,
    target_id bigint NOT NULL,
    label character varying(64),
    created_by bigint,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone
);


--
-- Name: resource_links_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.resource_links_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: resource_links_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.resource_links_id_seq OWNED BY public.resource_links.id;


--
-- Name: s3_storages; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.s3_storages (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    name character varying(255) NOT NULL,
    description text,
    region character varying(255) DEFAULT 'us-east-1'::character varying NOT NULL,
    key text NOT NULL,
    secret text NOT NULL,
    bucket text,
    endpoint text,
    team_id bigint NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    is_usable boolean DEFAULT false NOT NULL,
    unusable_email_sent boolean DEFAULT false NOT NULL,
    use_path_style_endpoint boolean DEFAULT true NOT NULL,
    type character varying(255) DEFAULT 's3'::character varying NOT NULL,
    private_key_id bigint,
    sftp_host_key text,
    skip_tls_verification boolean DEFAULT false NOT NULL
);


--
-- Name: s3_storages_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.s3_storages_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: s3_storages_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.s3_storages_id_seq OWNED BY public.s3_storages.id;


--
-- Name: scheduled_database_backup_executions; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.scheduled_database_backup_executions (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    status character varying(255) DEFAULT 'running'::character varying NOT NULL,
    message text,
    size text,
    filename text,
    scheduled_database_backup_id bigint NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    database_name character varying(255),
    finished_at timestamp(0) without time zone,
    local_storage_deleted boolean DEFAULT false NOT NULL,
    s3_storage_deleted boolean DEFAULT false NOT NULL,
    s3_uploaded boolean,
    worker_host character varying(255),
    CONSTRAINT scheduled_database_backup_executions_status_check CHECK (((status)::text = ANY ((ARRAY['success'::character varying, 'failed'::character varying, 'running'::character varying])::text[])))
);


--
-- Name: scheduled_database_backup_executions_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.scheduled_database_backup_executions_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: scheduled_database_backup_executions_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.scheduled_database_backup_executions_id_seq OWNED BY public.scheduled_database_backup_executions.id;


--
-- Name: scheduled_database_backup_s3_storage; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.scheduled_database_backup_s3_storage (
    id bigint NOT NULL,
    scheduled_database_backup_id bigint NOT NULL,
    s3_storage_id bigint NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone
);


--
-- Name: scheduled_database_backup_s3_storage_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.scheduled_database_backup_s3_storage_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: scheduled_database_backup_s3_storage_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.scheduled_database_backup_s3_storage_id_seq OWNED BY public.scheduled_database_backup_s3_storage.id;


--
-- Name: scheduled_database_backups; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.scheduled_database_backups (
    id bigint NOT NULL,
    description text,
    uuid character varying(255) NOT NULL,
    enabled boolean DEFAULT true NOT NULL,
    save_s3 boolean DEFAULT true NOT NULL,
    frequency character varying(255) NOT NULL,
    database_backup_retention_amount_locally integer DEFAULT 0 NOT NULL,
    database_type character varying(255) NOT NULL,
    database_id bigint NOT NULL,
    s3_storage_id bigint,
    team_id bigint NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    databases_to_backup text,
    dump_all boolean DEFAULT false NOT NULL,
    database_backup_retention_days_locally integer DEFAULT 0 NOT NULL,
    database_backup_retention_max_storage_locally numeric(17,7) DEFAULT '0'::numeric NOT NULL,
    database_backup_retention_amount_s3 integer DEFAULT 0 NOT NULL,
    database_backup_retention_days_s3 integer DEFAULT 0 NOT NULL,
    database_backup_retention_max_storage_s3 numeric(17,7) DEFAULT '0'::numeric NOT NULL,
    timeout integer DEFAULT 3600 NOT NULL,
    disable_local_backup boolean DEFAULT false NOT NULL,
    missing_backup_notification_days integer DEFAULT 0 NOT NULL,
    last_execution_at timestamp(0) without time zone
);


--
-- Name: scheduled_database_backups_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.scheduled_database_backups_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: scheduled_database_backups_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.scheduled_database_backups_id_seq OWNED BY public.scheduled_database_backups.id;


--
-- Name: scheduled_job_deliveries; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.scheduled_job_deliveries (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    schedule_key character varying(255) NOT NULL,
    scheduled_for timestamp(0) with time zone NOT NULL,
    job_type character varying(255) NOT NULL,
    resource_id bigint NOT NULL,
    payload json,
    status character varying(255) DEFAULT 'pending'::character varying NOT NULL,
    claim_token character varying(255),
    enqueued_at timestamp(0) with time zone,
    started_at timestamp(0) with time zone,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone
);


--
-- Name: scheduled_job_deliveries_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.scheduled_job_deliveries_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: scheduled_job_deliveries_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.scheduled_job_deliveries_id_seq OWNED BY public.scheduled_job_deliveries.id;


--
-- Name: scheduled_job_states; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.scheduled_job_states (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    schedule_key character varying(255) NOT NULL,
    last_scheduled_for timestamp(0) with time zone,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone
);


--
-- Name: scheduled_job_states_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.scheduled_job_states_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: scheduled_job_states_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.scheduled_job_states_id_seq OWNED BY public.scheduled_job_states.id;


--
-- Name: scheduled_task_executions; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.scheduled_task_executions (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    status character varying(255) DEFAULT 'running'::character varying NOT NULL,
    message text,
    scheduled_task_id bigint NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    finished_at timestamp(0) without time zone,
    started_at timestamp(0) without time zone,
    retry_count integer DEFAULT 0 NOT NULL,
    duration numeric(10,2),
    error_details text,
    worker_host character varying(255),
    CONSTRAINT scheduled_task_executions_status_check CHECK (((status)::text = ANY ((ARRAY['success'::character varying, 'failed'::character varying, 'running'::character varying, 'skipped'::character varying])::text[])))
);


--
-- Name: COLUMN scheduled_task_executions.duration; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.scheduled_task_executions.duration IS 'Duration in seconds';


--
-- Name: scheduled_task_executions_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.scheduled_task_executions_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: scheduled_task_executions_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.scheduled_task_executions_id_seq OWNED BY public.scheduled_task_executions.id;


--
-- Name: scheduled_tasks; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.scheduled_tasks (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    enabled boolean DEFAULT true NOT NULL,
    name character varying(255) NOT NULL,
    command character varying(255) NOT NULL,
    frequency character varying(255) NOT NULL,
    container character varying(255),
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    application_id bigint,
    service_id bigint,
    team_id bigint NOT NULL,
    timeout integer DEFAULT 300 NOT NULL,
    consecutive_failures integer DEFAULT 0 NOT NULL,
    failing_since timestamp(0) without time zone,
    disabled_reason text,
    use_shell boolean DEFAULT true NOT NULL
);


--
-- Name: scheduled_tasks_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.scheduled_tasks_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: scheduled_tasks_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.scheduled_tasks_id_seq OWNED BY public.scheduled_tasks.id;


--
-- Name: scheduled_volume_backup_executions; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.scheduled_volume_backup_executions (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    scheduled_volume_backup_id bigint NOT NULL,
    s3_storage_id bigint,
    status character varying(255) DEFAULT 'running'::character varying NOT NULL,
    message text,
    size bigint DEFAULT '0'::bigint NOT NULL,
    filename text,
    stop_container_ids json,
    stop_recovery_pending boolean DEFAULT false NOT NULL,
    s3_cleanup_pending boolean DEFAULT false NOT NULL,
    finished_at timestamp(0) without time zone,
    local_storage_deleted boolean DEFAULT false NOT NULL,
    s3_storage_deleted boolean DEFAULT false NOT NULL,
    s3_uploaded boolean,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    recovery_last_attempt_at timestamp(0) with time zone,
    recovery_error character varying(32),
    recovery_needs_attention boolean DEFAULT false NOT NULL,
    worker_host character varying(255),
    CONSTRAINT scheduled_volume_backup_executions_status_check CHECK (((status)::text = ANY ((ARRAY['success'::character varying, 'failed'::character varying, 'running'::character varying])::text[])))
);


--
-- Name: scheduled_volume_backup_executions_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.scheduled_volume_backup_executions_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: scheduled_volume_backup_executions_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.scheduled_volume_backup_executions_id_seq OWNED BY public.scheduled_volume_backup_executions.id;


--
-- Name: scheduled_volume_backup_s3_storage; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.scheduled_volume_backup_s3_storage (
    id bigint NOT NULL,
    scheduled_volume_backup_id bigint NOT NULL,
    s3_storage_id bigint NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone
);


--
-- Name: scheduled_volume_backup_s3_storage_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.scheduled_volume_backup_s3_storage_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: scheduled_volume_backup_s3_storage_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.scheduled_volume_backup_s3_storage_id_seq OWNED BY public.scheduled_volume_backup_s3_storage.id;


--
-- Name: scheduled_volume_backups; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.scheduled_volume_backups (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    backupable_type character varying(255) NOT NULL,
    backupable_id bigint NOT NULL,
    team_id bigint NOT NULL,
    s3_storage_id bigint,
    frequency character varying(255) NOT NULL,
    enabled boolean DEFAULT true NOT NULL,
    save_s3 boolean DEFAULT false NOT NULL,
    disable_local_backup boolean DEFAULT false NOT NULL,
    stop_during_backup boolean DEFAULT false NOT NULL,
    retention_amount_locally integer DEFAULT 7 NOT NULL,
    retention_days_locally integer DEFAULT 0 NOT NULL,
    retention_max_storage_locally numeric(17,7) DEFAULT '0'::numeric NOT NULL,
    retention_amount_s3 integer DEFAULT 7 NOT NULL,
    retention_days_s3 integer DEFAULT 0 NOT NULL,
    retention_max_storage_s3 numeric(17,7) DEFAULT '0'::numeric NOT NULL,
    timeout integer DEFAULT 36000 NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    missing_backup_notification_days integer DEFAULT 0 NOT NULL
);


--
-- Name: scheduled_volume_backups_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.scheduled_volume_backups_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: scheduled_volume_backups_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.scheduled_volume_backups_id_seq OWNED BY public.scheduled_volume_backups.id;


--
-- Name: secret_manager_links; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.secret_manager_links (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    resourceable_type character varying(255) NOT NULL,
    resourceable_id bigint NOT NULL,
    integration_token_id bigint NOT NULL,
    settings json,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone
);


--
-- Name: secret_manager_links_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.secret_manager_links_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: secret_manager_links_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.secret_manager_links_id_seq OWNED BY public.secret_manager_links.id;


--
-- Name: server_host_scans; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.server_host_scans (
    id bigint NOT NULL,
    server_id bigint NOT NULL,
    error_code character varying(255),
    error_context jsonb,
    eol jsonb,
    summary jsonb,
    findings jsonb,
    findings_truncated boolean DEFAULT false NOT NULL,
    grype_db_built_at timestamp(0) without time zone,
    scanned_at timestamp(0) without time zone,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone
);


--
-- Name: server_host_scans_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.server_host_scans_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: server_host_scans_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.server_host_scans_id_seq OWNED BY public.server_host_scans.id;


--
-- Name: server_settings; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.server_settings (
    id bigint NOT NULL,
    is_swarm_manager boolean DEFAULT false NOT NULL,
    is_jump_server boolean DEFAULT false NOT NULL,
    is_build_server boolean DEFAULT false NOT NULL,
    is_reachable boolean DEFAULT false NOT NULL,
    is_usable boolean DEFAULT false NOT NULL,
    server_id bigint NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    wildcard_domain character varying(255),
    is_cloudflare_tunnel boolean DEFAULT false NOT NULL,
    is_logdrain_newrelic_enabled boolean DEFAULT false NOT NULL,
    logdrain_newrelic_license_key character varying(255),
    logdrain_newrelic_base_uri character varying(255),
    is_logdrain_axiom_enabled boolean DEFAULT false NOT NULL,
    logdrain_axiom_dataset_name character varying(255),
    logdrain_axiom_api_key character varying(255),
    is_swarm_worker boolean DEFAULT false NOT NULL,
    is_logdrain_custom_enabled boolean DEFAULT false NOT NULL,
    logdrain_custom_config text,
    logdrain_custom_config_parser text,
    concurrent_builds integer DEFAULT 2 NOT NULL,
    dynamic_timeout integer DEFAULT 3600 NOT NULL,
    force_disabled boolean DEFAULT false NOT NULL,
    is_metrics_enabled boolean DEFAULT false NOT NULL,
    generate_exact_labels boolean DEFAULT false NOT NULL,
    force_docker_cleanup boolean DEFAULT true NOT NULL,
    docker_cleanup_frequency character varying(255) DEFAULT '0 0 * * *'::character varying NOT NULL,
    docker_cleanup_threshold integer DEFAULT 80 NOT NULL,
    server_timezone character varying(255) DEFAULT 'UTC'::character varying NOT NULL,
    delete_unused_volumes boolean DEFAULT false NOT NULL,
    delete_unused_networks boolean DEFAULT false NOT NULL,
    is_sentinel_enabled boolean DEFAULT true NOT NULL,
    sentinel_token text,
    sentinel_metrics_refresh_rate_seconds integer DEFAULT 10 NOT NULL,
    sentinel_metrics_history_days integer DEFAULT 7 NOT NULL,
    sentinel_push_interval_seconds integer DEFAULT 60 NOT NULL,
    sentinel_custom_url character varying(255),
    server_disk_usage_notification_threshold integer DEFAULT 80 NOT NULL,
    is_sentinel_debug_enabled boolean DEFAULT false NOT NULL,
    server_disk_usage_check_frequency character varying(255) DEFAULT '0 23 * * *'::character varying NOT NULL,
    is_terminal_enabled boolean DEFAULT true NOT NULL,
    deployment_queue_limit integer DEFAULT 25 NOT NULL,
    disable_application_image_retention boolean DEFAULT false NOT NULL,
    connection_timeout integer DEFAULT 10 NOT NULL,
    is_traffic_analytics_enabled boolean DEFAULT false NOT NULL,
    geoip_maxmind_license_key text,
    traffic_topn integer DEFAULT 50 NOT NULL,
    traffic_sample_threshold integer DEFAULT 0 NOT NULL,
    traffic_retention_1h_days integer DEFAULT 30 NOT NULL,
    traffic_retention_1d_days integer DEFAULT 395 NOT NULL,
    is_geoip_enabled boolean DEFAULT true NOT NULL,
    geoip_refresh_days integer DEFAULT 30 NOT NULL,
    docker_version character varying(255),
    docker_version_checked_at timestamp(0) without time zone,
    compose_version character varying(255),
    compose_version_checked_at timestamp(0) without time zone,
    backup_compression_cpu_percentage smallint DEFAULT '25'::smallint NOT NULL,
    server_role character varying(255) DEFAULT 'both'::character varying,
    is_logdrain_cloudwatch_enabled boolean DEFAULT false NOT NULL,
    logdrain_cloudwatch_region character varying(255),
    logdrain_cloudwatch_group character varying(512),
    logdrain_cloudwatch_stream_prefix character varying(255),
    server_disk_usage_notification_interval_hours integer DEFAULT 24 NOT NULL,
    traffic_ip_mode character varying(255) DEFAULT 'full'::character varying NOT NULL,
    cloudflare_tunnel_id character varying(255),
    cloudflare_tunnel_account_id character varying(255),
    cloudflare_tunnel_integration_token_id bigint,
    traefik_certificate_resolver character varying(255) DEFAULT 'letsencrypt'::character varying,
    patch_check_frequency character varying(255),
    patch_check_result json,
    patch_check_error text,
    patch_checked_at timestamp(0) without time zone,
    is_auto_security_updates_enabled boolean DEFAULT false NOT NULL,
    auto_security_updates_time character varying(255) DEFAULT '03:00'::character varying NOT NULL,
    logdrain_axiom_domain character varying(255) DEFAULT 'us-east-1.aws.edge.axiom.co'::character varying,
    is_image_scan_enabled boolean DEFAULT false NOT NULL,
    image_candidate_scans_today integer DEFAULT 0 NOT NULL,
    image_candidate_scans_date date,
    ssh_host_key text,
    ssh_host_key_source character varying(255),
    ssh_host_key_trusted_at timestamp(0) without time zone,
    ssh_host_key_pending text,
    is_ssh_host_key_check_disabled boolean DEFAULT false NOT NULL,
    is_ssh_host_key_check_enforced boolean DEFAULT false NOT NULL,
    image_scan_memory_mb integer DEFAULT 1024 NOT NULL,
    image_scan_syft_timeout integer DEFAULT 600 NOT NULL,
    image_scan_grype_timeout integer DEFAULT 600 NOT NULL,
    image_scan_cpus numeric(5,2) DEFAULT '1'::numeric NOT NULL
);


--
-- Name: server_settings_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.server_settings_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: server_settings_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.server_settings_id_seq OWNED BY public.server_settings.id;


--
-- Name: servers; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.servers (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    name character varying(255) NOT NULL,
    description character varying(255),
    ip character varying(255) NOT NULL,
    port integer DEFAULT 22 NOT NULL,
    "user" character varying(255) DEFAULT 'root'::character varying NOT NULL,
    team_id bigint NOT NULL,
    private_key_id bigint NOT NULL,
    proxy json,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    unreachable_count integer DEFAULT 0 NOT NULL,
    high_disk_usage_notification_sent boolean DEFAULT false NOT NULL,
    log_drain_notification_sent boolean DEFAULT false NOT NULL,
    swarm_cluster integer,
    validation_logs text,
    sentinel_updated_at timestamp(0) without time zone DEFAULT '2026-11-13 08:05:19'::timestamp without time zone NOT NULL,
    deleted_at timestamp(0) without time zone,
    ip_previous character varying(255),
    hetzner_server_id bigint,
    cloud_provider_token_id bigint,
    hetzner_server_status character varying(255),
    is_validating boolean DEFAULT false NOT NULL,
    detected_traefik_version character varying(255),
    traefik_outdated_info json,
    server_metadata json,
    vultr_instance_id character varying(255),
    vultr_instance_status character varying(255),
    digitalocean_droplet_id bigint,
    digitalocean_droplet_status character varying(255),
    hostinger_virtual_machine_id bigint,
    hostinger_virtual_machine_status character varying(255),
    sentinel_waiting_since timestamp(0) without time zone,
    proxy_error_page text,
    canvas_position json
)
WITH (fillfactor='85', autovacuum_vacuum_scale_factor='0.05');


--
-- Name: servers_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.servers_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: servers_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.servers_id_seq OWNED BY public.servers.id;


--
-- Name: service_applications; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.service_applications (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    name character varying(255) NOT NULL,
    human_name character varying(255),
    description text,
    fqdn text,
    ports text,
    exposes text,
    status character varying(255) DEFAULT 'exited'::character varying NOT NULL,
    service_id bigint NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    exclude_from_status boolean DEFAULT false NOT NULL,
    required_fqdn boolean DEFAULT false NOT NULL,
    image character varying(255),
    is_log_drain_enabled boolean DEFAULT false NOT NULL,
    is_include_timestamps boolean DEFAULT false NOT NULL,
    deleted_at timestamp(0) without time zone,
    is_gzip_enabled boolean DEFAULT true NOT NULL,
    is_stripprefix_enabled boolean DEFAULT true NOT NULL,
    last_online_at timestamp(0) without time zone DEFAULT '2026-11-13 08:05:19'::timestamp without time zone NOT NULL,
    is_migrated boolean DEFAULT false NOT NULL,
    noindex_domains json,
    domain_dns_statuses json,
    redirect character varying(255) DEFAULT 'both'::character varying NOT NULL,
    is_force_https_enabled boolean DEFAULT true NOT NULL,
    restart_count integer DEFAULT 0 NOT NULL,
    max_restart_count integer DEFAULT 0 NOT NULL,
    restart_limit_reached boolean DEFAULT false NOT NULL,
    last_restart_at timestamp(0) without time zone,
    last_restart_type character varying(10),
    domain_port_overrides json,
    traefik_certificate_resolver character varying(255),
    is_permanent_redirect_enabled boolean DEFAULT false NOT NULL,
    is_removed_from_compose boolean DEFAULT false NOT NULL,
    CONSTRAINT service_applications_redirect_check CHECK (((redirect)::text = ANY ((ARRAY['www'::character varying, 'non-www'::character varying, 'both'::character varying])::text[])))
)
WITH (fillfactor='85', autovacuum_vacuum_scale_factor='0.05');


--
-- Name: service_applications_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.service_applications_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: service_applications_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.service_applications_id_seq OWNED BY public.service_applications.id;


--
-- Name: service_databases; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.service_databases (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    name character varying(255) NOT NULL,
    human_name character varying(255),
    description text,
    ports text,
    exposes text,
    status character varying(255) DEFAULT 'exited'::character varying NOT NULL,
    service_id bigint NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    exclude_from_status boolean DEFAULT false NOT NULL,
    image character varying(255),
    public_port integer,
    is_public boolean DEFAULT false NOT NULL,
    is_log_drain_enabled boolean DEFAULT false NOT NULL,
    is_include_timestamps boolean DEFAULT false NOT NULL,
    deleted_at timestamp(0) without time zone,
    is_gzip_enabled boolean DEFAULT true NOT NULL,
    is_stripprefix_enabled boolean DEFAULT true NOT NULL,
    last_online_at timestamp(0) without time zone DEFAULT '2026-11-13 08:05:19'::timestamp without time zone NOT NULL,
    is_migrated boolean DEFAULT false NOT NULL,
    custom_type character varying(255),
    public_port_timeout integer DEFAULT 3600,
    is_removed_from_compose boolean DEFAULT false NOT NULL
)
WITH (fillfactor='85', autovacuum_vacuum_scale_factor='0.05');


--
-- Name: service_databases_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.service_databases_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: service_databases_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.service_databases_id_seq OWNED BY public.service_databases.id;


--
-- Name: services; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.services (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    name character varying(255) NOT NULL,
    environment_id bigint NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    server_id bigint,
    description text,
    docker_compose_raw text NOT NULL,
    docker_compose text,
    destination_type character varying(255),
    destination_id bigint,
    deleted_at timestamp(0) without time zone,
    connect_to_docker_network boolean DEFAULT false NOT NULL,
    config_hash character varying(255),
    service_type character varying(255),
    is_container_label_escape_enabled boolean DEFAULT true NOT NULL,
    compose_parsing_version character varying(255) DEFAULT '2'::character varying NOT NULL,
    redact_all_env_values_in_logs boolean DEFAULT true NOT NULL,
    is_maintenance_enabled boolean DEFAULT false NOT NULL,
    maintenance_page text,
    include_coolify_default_variables boolean DEFAULT true NOT NULL
)
WITH (fillfactor='85');


--
-- Name: services_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.services_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: services_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.services_id_seq OWNED BY public.services.id;


--
-- Name: sessions; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.sessions (
    id character varying(255) NOT NULL,
    user_id bigint,
    ip_address character varying(45),
    user_agent text,
    payload text NOT NULL,
    last_activity integer NOT NULL
);


--
-- Name: shared_environment_variables; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.shared_environment_variables (
    id bigint NOT NULL,
    key character varying(255) NOT NULL,
    value text NOT NULL,
    is_shown_once boolean DEFAULT false NOT NULL,
    type character varying(255) DEFAULT 'team'::character varying NOT NULL,
    team_id bigint NOT NULL,
    project_id bigint,
    environment_id bigint,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    is_multiline boolean DEFAULT false NOT NULL,
    version character varying(255) DEFAULT '4.0.0-beta.239'::character varying NOT NULL,
    is_literal boolean DEFAULT false NOT NULL,
    comment character varying(256),
    server_id bigint,
    CONSTRAINT shared_environment_variables_type_check CHECK (((type)::text = ANY ((ARRAY['team'::character varying, 'project'::character varying, 'environment'::character varying, 'server'::character varying])::text[])))
);


--
-- Name: shared_environment_variables_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.shared_environment_variables_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: shared_environment_variables_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.shared_environment_variables_id_seq OWNED BY public.shared_environment_variables.id;


--
-- Name: slack_notification_settings; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.slack_notification_settings (
    id bigint NOT NULL,
    team_id bigint NOT NULL,
    slack_enabled boolean DEFAULT false NOT NULL,
    slack_webhook_url text,
    deployment_success_slack_notifications boolean DEFAULT false NOT NULL,
    deployment_failure_slack_notifications boolean DEFAULT true NOT NULL,
    status_change_slack_notifications boolean DEFAULT false NOT NULL,
    backup_success_slack_notifications boolean DEFAULT false NOT NULL,
    backup_failure_slack_notifications boolean DEFAULT true NOT NULL,
    scheduled_task_success_slack_notifications boolean DEFAULT false NOT NULL,
    scheduled_task_failure_slack_notifications boolean DEFAULT true NOT NULL,
    docker_cleanup_success_slack_notifications boolean DEFAULT false NOT NULL,
    docker_cleanup_failure_slack_notifications boolean DEFAULT true NOT NULL,
    server_disk_usage_slack_notifications boolean DEFAULT true NOT NULL,
    server_reachable_slack_notifications boolean DEFAULT false NOT NULL,
    server_unreachable_slack_notifications boolean DEFAULT true NOT NULL,
    server_patch_slack_notifications boolean DEFAULT true NOT NULL,
    traefik_outdated_slack_notifications boolean DEFAULT true NOT NULL,
    restart_limit_reached_slack_notifications boolean DEFAULT true NOT NULL,
    failure_count integer DEFAULT 0 NOT NULL,
    image_vulnerabilities_slack_notifications boolean DEFAULT false NOT NULL
);


--
-- Name: slack_notification_settings_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.slack_notification_settings_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: slack_notification_settings_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.slack_notification_settings_id_seq OWNED BY public.slack_notification_settings.id;


--
-- Name: ssl_certificates; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.ssl_certificates (
    id bigint NOT NULL,
    ssl_certificate text NOT NULL,
    ssl_private_key text NOT NULL,
    configuration_dir text,
    mount_path text,
    resource_type character varying(255),
    resource_id bigint,
    server_id bigint NOT NULL,
    common_name text NOT NULL,
    subject_alternative_names json,
    valid_until timestamp(0) without time zone NOT NULL,
    is_ca_certificate boolean DEFAULT false NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone
);


--
-- Name: ssl_certificates_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.ssl_certificates_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: ssl_certificates_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.ssl_certificates_id_seq OWNED BY public.ssl_certificates.id;


--
-- Name: standalone_clickhouses; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.standalone_clickhouses (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    name character varying(255) NOT NULL,
    description character varying(255),
    clickhouse_admin_user character varying(255) DEFAULT 'default'::character varying NOT NULL,
    clickhouse_admin_password text NOT NULL,
    is_log_drain_enabled boolean DEFAULT false NOT NULL,
    is_include_timestamps boolean DEFAULT false NOT NULL,
    deleted_at timestamp(0) without time zone,
    status character varying(255) DEFAULT 'exited'::character varying NOT NULL,
    image character varying(255) DEFAULT 'clickhouse/clickhouse-server:26.8'::character varying NOT NULL,
    is_public boolean DEFAULT false NOT NULL,
    public_port integer,
    ports_mappings text,
    limits_memory character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_memory_swap character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_memory_swappiness integer DEFAULT 60 NOT NULL,
    limits_memory_reservation character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_cpus character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_cpuset character varying(255),
    limits_cpu_shares integer DEFAULT 1024 NOT NULL,
    started_at timestamp(0) without time zone,
    destination_type character varying(255) NOT NULL,
    destination_id bigint NOT NULL,
    environment_id bigint,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    config_hash character varying(255),
    custom_docker_run_options text,
    last_online_at timestamp(0) without time zone DEFAULT '2026-11-13 08:05:19'::timestamp without time zone NOT NULL,
    clickhouse_db character varying(255) DEFAULT 'default'::character varying NOT NULL,
    restart_count integer DEFAULT 0 NOT NULL,
    last_restart_at timestamp(0) without time zone,
    last_restart_type character varying(10),
    public_port_timeout integer DEFAULT 3600,
    health_check_enabled boolean DEFAULT true NOT NULL,
    health_check_interval integer DEFAULT 15 NOT NULL,
    health_check_timeout integer DEFAULT 5 NOT NULL,
    health_check_retries integer DEFAULT 5 NOT NULL,
    health_check_start_period integer DEFAULT 5 NOT NULL,
    redact_all_env_values_in_logs boolean DEFAULT true NOT NULL,
    include_coolify_default_variables boolean DEFAULT true NOT NULL
)
WITH (fillfactor='85');


--
-- Name: standalone_clickhouses_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.standalone_clickhouses_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: standalone_clickhouses_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.standalone_clickhouses_id_seq OWNED BY public.standalone_clickhouses.id;


--
-- Name: standalone_dockers; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.standalone_dockers (
    id bigint NOT NULL,
    name character varying(255) NOT NULL,
    uuid character varying(255) NOT NULL,
    network character varying(255) NOT NULL,
    server_id bigint NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone
);


--
-- Name: standalone_dockers_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.standalone_dockers_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: standalone_dockers_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.standalone_dockers_id_seq OWNED BY public.standalone_dockers.id;


--
-- Name: standalone_dragonflies; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.standalone_dragonflies (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    name character varying(255) NOT NULL,
    description character varying(255),
    dragonfly_password text NOT NULL,
    is_log_drain_enabled boolean DEFAULT false NOT NULL,
    is_include_timestamps boolean DEFAULT false NOT NULL,
    deleted_at timestamp(0) without time zone,
    status character varying(255) DEFAULT 'exited'::character varying NOT NULL,
    image character varying(255) DEFAULT 'docker.dragonflydb.io/dragonflydb/dragonfly'::character varying NOT NULL,
    is_public boolean DEFAULT false NOT NULL,
    public_port integer,
    ports_mappings text,
    limits_memory character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_memory_swap character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_memory_swappiness integer DEFAULT 60 NOT NULL,
    limits_memory_reservation character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_cpus character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_cpuset character varying(255),
    limits_cpu_shares integer DEFAULT 1024 NOT NULL,
    started_at timestamp(0) without time zone,
    destination_type character varying(255) NOT NULL,
    destination_id bigint NOT NULL,
    environment_id bigint,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    config_hash character varying(255),
    custom_docker_run_options text,
    last_online_at timestamp(0) without time zone DEFAULT '2026-11-13 08:05:19'::timestamp without time zone NOT NULL,
    enable_ssl boolean DEFAULT false NOT NULL,
    restart_count integer DEFAULT 0 NOT NULL,
    last_restart_at timestamp(0) without time zone,
    last_restart_type character varying(10),
    public_port_timeout integer DEFAULT 3600,
    health_check_enabled boolean DEFAULT true NOT NULL,
    health_check_interval integer DEFAULT 15 NOT NULL,
    health_check_timeout integer DEFAULT 5 NOT NULL,
    health_check_retries integer DEFAULT 5 NOT NULL,
    health_check_start_period integer DEFAULT 5 NOT NULL,
    legacy_password_quoting boolean DEFAULT false NOT NULL,
    redact_all_env_values_in_logs boolean DEFAULT true NOT NULL,
    include_coolify_default_variables boolean DEFAULT true NOT NULL
)
WITH (fillfactor='85');


--
-- Name: standalone_dragonflies_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.standalone_dragonflies_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: standalone_dragonflies_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.standalone_dragonflies_id_seq OWNED BY public.standalone_dragonflies.id;


--
-- Name: standalone_keydbs; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.standalone_keydbs (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    name character varying(255) NOT NULL,
    description character varying(255),
    keydb_password text NOT NULL,
    keydb_conf text,
    is_log_drain_enabled boolean DEFAULT false NOT NULL,
    is_include_timestamps boolean DEFAULT false NOT NULL,
    deleted_at timestamp(0) without time zone,
    status character varying(255) DEFAULT 'exited'::character varying NOT NULL,
    image character varying(255) DEFAULT 'eqalpha/keydb:latest'::character varying NOT NULL,
    is_public boolean DEFAULT false NOT NULL,
    public_port integer,
    ports_mappings text,
    limits_memory character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_memory_swap character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_memory_swappiness integer DEFAULT 60 NOT NULL,
    limits_memory_reservation character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_cpus character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_cpuset character varying(255),
    limits_cpu_shares integer DEFAULT 1024 NOT NULL,
    started_at timestamp(0) without time zone,
    destination_type character varying(255) NOT NULL,
    destination_id bigint NOT NULL,
    environment_id bigint,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    config_hash character varying(255),
    custom_docker_run_options text,
    last_online_at timestamp(0) without time zone DEFAULT '2026-11-13 08:05:19'::timestamp without time zone NOT NULL,
    enable_ssl boolean DEFAULT false NOT NULL,
    restart_count integer DEFAULT 0 NOT NULL,
    last_restart_at timestamp(0) without time zone,
    last_restart_type character varying(10),
    public_port_timeout integer DEFAULT 3600,
    health_check_enabled boolean DEFAULT true NOT NULL,
    health_check_interval integer DEFAULT 15 NOT NULL,
    health_check_timeout integer DEFAULT 5 NOT NULL,
    health_check_retries integer DEFAULT 5 NOT NULL,
    health_check_start_period integer DEFAULT 5 NOT NULL,
    legacy_password_quoting boolean DEFAULT false NOT NULL,
    redact_all_env_values_in_logs boolean DEFAULT true NOT NULL,
    include_coolify_default_variables boolean DEFAULT true NOT NULL
)
WITH (fillfactor='85');


--
-- Name: standalone_keydbs_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.standalone_keydbs_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: standalone_keydbs_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.standalone_keydbs_id_seq OWNED BY public.standalone_keydbs.id;


--
-- Name: standalone_mariadbs; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.standalone_mariadbs (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    name character varying(255) NOT NULL,
    description character varying(255),
    mariadb_root_password text NOT NULL,
    mariadb_user character varying(255) DEFAULT 'mariadb'::character varying NOT NULL,
    mariadb_password text NOT NULL,
    mariadb_database character varying(255) DEFAULT 'default'::character varying NOT NULL,
    mariadb_conf text,
    status character varying(255) DEFAULT 'exited'::character varying NOT NULL,
    image character varying(255) DEFAULT 'mariadb:12.3'::character varying NOT NULL,
    is_public boolean DEFAULT false NOT NULL,
    public_port integer,
    ports_mappings text,
    limits_memory character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_memory_swap character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_memory_swappiness integer DEFAULT 60 NOT NULL,
    limits_memory_reservation character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_cpus character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_cpuset character varying(255),
    limits_cpu_shares integer DEFAULT 1024 NOT NULL,
    started_at timestamp(0) without time zone,
    destination_type character varying(255) NOT NULL,
    destination_id bigint NOT NULL,
    environment_id bigint,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    is_log_drain_enabled boolean DEFAULT false NOT NULL,
    deleted_at timestamp(0) without time zone,
    config_hash character varying(255),
    custom_docker_run_options text,
    last_online_at timestamp(0) without time zone DEFAULT '2026-11-13 08:05:19'::timestamp without time zone NOT NULL,
    enable_ssl boolean DEFAULT false NOT NULL,
    restart_count integer DEFAULT 0 NOT NULL,
    last_restart_at timestamp(0) without time zone,
    last_restart_type character varying(10),
    public_port_timeout integer DEFAULT 3600,
    health_check_enabled boolean DEFAULT true NOT NULL,
    health_check_interval integer DEFAULT 15 NOT NULL,
    health_check_timeout integer DEFAULT 5 NOT NULL,
    health_check_retries integer DEFAULT 5 NOT NULL,
    health_check_start_period integer DEFAULT 5 NOT NULL,
    redact_all_env_values_in_logs boolean DEFAULT true NOT NULL,
    include_coolify_default_variables boolean DEFAULT true NOT NULL
)
WITH (fillfactor='85');


--
-- Name: standalone_mariadbs_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.standalone_mariadbs_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: standalone_mariadbs_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.standalone_mariadbs_id_seq OWNED BY public.standalone_mariadbs.id;


--
-- Name: standalone_mongodbs; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.standalone_mongodbs (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    name character varying(255) NOT NULL,
    description character varying(255),
    mongo_conf text,
    mongo_initdb_root_username text DEFAULT 'root'::text NOT NULL,
    mongo_initdb_root_password text NOT NULL,
    mongo_initdb_database text DEFAULT 'default'::text NOT NULL,
    status character varying(255) DEFAULT 'exited'::character varying NOT NULL,
    image character varying(255) DEFAULT 'mongo:9'::character varying NOT NULL,
    is_public boolean DEFAULT false NOT NULL,
    public_port integer,
    ports_mappings text,
    limits_memory character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_memory_swap character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_memory_swappiness integer DEFAULT 60 NOT NULL,
    limits_memory_reservation character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_cpus character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_cpuset character varying(255),
    limits_cpu_shares integer DEFAULT 1024 NOT NULL,
    started_at timestamp(0) without time zone,
    destination_type character varying(255) NOT NULL,
    destination_id bigint NOT NULL,
    environment_id bigint,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    is_log_drain_enabled boolean DEFAULT false NOT NULL,
    is_include_timestamps boolean DEFAULT false NOT NULL,
    deleted_at timestamp(0) without time zone,
    config_hash character varying(255),
    custom_docker_run_options text,
    last_online_at timestamp(0) without time zone DEFAULT '2026-11-13 08:05:19'::timestamp without time zone NOT NULL,
    enable_ssl boolean DEFAULT false NOT NULL,
    ssl_mode character varying(255) DEFAULT 'require'::character varying NOT NULL,
    restart_count integer DEFAULT 0 NOT NULL,
    last_restart_at timestamp(0) without time zone,
    last_restart_type character varying(10),
    public_port_timeout integer DEFAULT 3600,
    health_check_enabled boolean DEFAULT true NOT NULL,
    health_check_interval integer DEFAULT 15 NOT NULL,
    health_check_timeout integer DEFAULT 5 NOT NULL,
    health_check_retries integer DEFAULT 5 NOT NULL,
    health_check_start_period integer DEFAULT 5 NOT NULL,
    redact_all_env_values_in_logs boolean DEFAULT true NOT NULL,
    include_coolify_default_variables boolean DEFAULT true NOT NULL,
    CONSTRAINT standalone_mongodbs_ssl_mode_check CHECK (((ssl_mode)::text = ANY ((ARRAY['allow'::character varying, 'prefer'::character varying, 'require'::character varying, 'verify-full'::character varying])::text[])))
)
WITH (fillfactor='85');


--
-- Name: standalone_mongodbs_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.standalone_mongodbs_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: standalone_mongodbs_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.standalone_mongodbs_id_seq OWNED BY public.standalone_mongodbs.id;


--
-- Name: standalone_mysqls; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.standalone_mysqls (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    name character varying(255) NOT NULL,
    description character varying(255),
    mysql_root_password text NOT NULL,
    mysql_user character varying(255) DEFAULT 'mysql'::character varying NOT NULL,
    mysql_password text NOT NULL,
    mysql_database character varying(255) DEFAULT 'default'::character varying NOT NULL,
    mysql_conf text,
    status character varying(255) DEFAULT 'exited'::character varying NOT NULL,
    image character varying(255) DEFAULT 'mysql:9.7'::character varying NOT NULL,
    is_public boolean DEFAULT false NOT NULL,
    public_port integer,
    ports_mappings text,
    limits_memory character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_memory_swap character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_memory_swappiness integer DEFAULT 60 NOT NULL,
    limits_memory_reservation character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_cpus character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_cpuset character varying(255),
    limits_cpu_shares integer DEFAULT 1024 NOT NULL,
    started_at timestamp(0) without time zone,
    destination_type character varying(255) NOT NULL,
    destination_id bigint NOT NULL,
    environment_id bigint,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    is_log_drain_enabled boolean DEFAULT false NOT NULL,
    is_include_timestamps boolean DEFAULT false NOT NULL,
    deleted_at timestamp(0) without time zone,
    config_hash character varying(255),
    custom_docker_run_options text,
    last_online_at timestamp(0) without time zone DEFAULT '2026-11-13 08:05:19'::timestamp without time zone NOT NULL,
    enable_ssl boolean DEFAULT false NOT NULL,
    ssl_mode character varying(255) DEFAULT 'REQUIRED'::character varying NOT NULL,
    restart_count integer DEFAULT 0 NOT NULL,
    last_restart_at timestamp(0) without time zone,
    last_restart_type character varying(10),
    public_port_timeout integer DEFAULT 3600,
    health_check_enabled boolean DEFAULT true NOT NULL,
    health_check_interval integer DEFAULT 15 NOT NULL,
    health_check_timeout integer DEFAULT 5 NOT NULL,
    health_check_retries integer DEFAULT 5 NOT NULL,
    health_check_start_period integer DEFAULT 5 NOT NULL,
    redact_all_env_values_in_logs boolean DEFAULT true NOT NULL,
    include_coolify_default_variables boolean DEFAULT true NOT NULL,
    CONSTRAINT standalone_mysqls_ssl_mode_check CHECK (((ssl_mode)::text = ANY ((ARRAY['PREFERRED'::character varying, 'REQUIRED'::character varying, 'VERIFY_CA'::character varying, 'VERIFY_IDENTITY'::character varying])::text[])))
)
WITH (fillfactor='85');


--
-- Name: standalone_mysqls_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.standalone_mysqls_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: standalone_mysqls_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.standalone_mysqls_id_seq OWNED BY public.standalone_mysqls.id;


--
-- Name: standalone_postgresqls; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.standalone_postgresqls (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    name character varying(255) NOT NULL,
    description character varying(255),
    postgres_user character varying(255) DEFAULT 'postgres'::character varying NOT NULL,
    postgres_password text NOT NULL,
    postgres_db character varying(255) DEFAULT 'postgres'::character varying NOT NULL,
    postgres_initdb_args character varying(255),
    postgres_host_auth_method character varying(255),
    init_scripts json,
    status character varying(255) DEFAULT 'exited'::character varying NOT NULL,
    image character varying(255) DEFAULT 'postgres:18-alpine'::character varying NOT NULL,
    is_public boolean DEFAULT false NOT NULL,
    public_port integer,
    ports_mappings text,
    limits_memory character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_memory_swap character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_memory_swappiness integer DEFAULT 60 NOT NULL,
    limits_memory_reservation character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_cpus character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_cpuset character varying(255),
    limits_cpu_shares integer DEFAULT 1024 NOT NULL,
    started_at timestamp(0) without time zone,
    destination_type character varying(255) NOT NULL,
    destination_id bigint NOT NULL,
    environment_id bigint,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    postgres_conf text,
    is_log_drain_enabled boolean DEFAULT false NOT NULL,
    is_include_timestamps boolean DEFAULT false NOT NULL,
    deleted_at timestamp(0) without time zone,
    config_hash character varying(255),
    custom_docker_run_options text,
    last_online_at timestamp(0) without time zone DEFAULT '2026-11-13 08:05:19'::timestamp without time zone NOT NULL,
    enable_ssl boolean DEFAULT false NOT NULL,
    ssl_mode character varying(255) DEFAULT 'require'::character varying NOT NULL,
    restart_count integer DEFAULT 0 NOT NULL,
    last_restart_at timestamp(0) without time zone,
    last_restart_type character varying(10),
    public_port_timeout integer DEFAULT 3600,
    health_check_enabled boolean DEFAULT true NOT NULL,
    health_check_interval integer DEFAULT 15 NOT NULL,
    health_check_timeout integer DEFAULT 5 NOT NULL,
    health_check_retries integer DEFAULT 5 NOT NULL,
    health_check_start_period integer DEFAULT 5 NOT NULL,
    redact_all_env_values_in_logs boolean DEFAULT true NOT NULL,
    include_coolify_default_variables boolean DEFAULT true NOT NULL,
    CONSTRAINT standalone_postgresqls_ssl_mode_check CHECK (((ssl_mode)::text = ANY ((ARRAY['allow'::character varying, 'prefer'::character varying, 'require'::character varying, 'verify-ca'::character varying, 'verify-full'::character varying])::text[])))
)
WITH (fillfactor='85', autovacuum_vacuum_scale_factor='0.05');


--
-- Name: standalone_postgresqls_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.standalone_postgresqls_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: standalone_postgresqls_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.standalone_postgresqls_id_seq OWNED BY public.standalone_postgresqls.id;


--
-- Name: standalone_redis; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.standalone_redis (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    name character varying(255) NOT NULL,
    description character varying(255),
    redis_conf text,
    status character varying(255) DEFAULT 'exited'::character varying NOT NULL,
    image character varying(255) DEFAULT 'valkey/valkey:9-alpine'::character varying NOT NULL,
    is_public boolean DEFAULT false NOT NULL,
    public_port integer,
    ports_mappings text,
    limits_memory character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_memory_swap character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_memory_swappiness integer DEFAULT 60 NOT NULL,
    limits_memory_reservation character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_cpus character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_cpuset character varying(255),
    limits_cpu_shares integer DEFAULT 1024 NOT NULL,
    started_at timestamp(0) without time zone,
    destination_type character varying(255) NOT NULL,
    destination_id bigint NOT NULL,
    environment_id bigint,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    is_log_drain_enabled boolean DEFAULT false NOT NULL,
    is_include_timestamps boolean DEFAULT false NOT NULL,
    deleted_at timestamp(0) without time zone,
    config_hash character varying(255),
    custom_docker_run_options text,
    last_online_at timestamp(0) without time zone DEFAULT '2026-11-13 08:05:19'::timestamp without time zone NOT NULL,
    enable_ssl boolean DEFAULT false NOT NULL,
    restart_count integer DEFAULT 0 NOT NULL,
    last_restart_at timestamp(0) without time zone,
    last_restart_type character varying(10),
    public_port_timeout integer DEFAULT 3600,
    health_check_enabled boolean DEFAULT true NOT NULL,
    health_check_interval integer DEFAULT 15 NOT NULL,
    health_check_timeout integer DEFAULT 5 NOT NULL,
    health_check_retries integer DEFAULT 5 NOT NULL,
    health_check_start_period integer DEFAULT 5 NOT NULL,
    legacy_password_quoting boolean DEFAULT false NOT NULL,
    redact_all_env_values_in_logs boolean DEFAULT true NOT NULL,
    include_coolify_default_variables boolean DEFAULT true NOT NULL
)
WITH (fillfactor='85');


--
-- Name: standalone_redis_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.standalone_redis_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: standalone_redis_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.standalone_redis_id_seq OWNED BY public.standalone_redis.id;


--
-- Name: standalone_sqlites; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.standalone_sqlites (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    name character varying(255) NOT NULL,
    description character varying(255),
    sqlite_databases character varying(255) DEFAULT 'database.sqlite'::character varying NOT NULL,
    is_log_drain_enabled boolean DEFAULT false NOT NULL,
    is_include_timestamps boolean DEFAULT false NOT NULL,
    deleted_at timestamp(0) without time zone,
    status character varying(255) DEFAULT 'exited'::character varying NOT NULL,
    restart_count integer DEFAULT 0 NOT NULL,
    last_restart_at timestamp(0) without time zone,
    last_restart_type character varying(10),
    image character varying(255) NOT NULL,
    is_public boolean DEFAULT false NOT NULL,
    limits_memory character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_memory_swap character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_memory_swappiness integer DEFAULT 60 NOT NULL,
    limits_memory_reservation character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_cpus character varying(255) DEFAULT '0'::character varying NOT NULL,
    limits_cpuset character varying(255),
    limits_cpu_shares integer DEFAULT 1024 NOT NULL,
    started_at timestamp(0) without time zone,
    destination_type character varying(255) NOT NULL,
    destination_id bigint NOT NULL,
    environment_id bigint,
    config_hash character varying(255),
    custom_docker_run_options text,
    health_check_enabled boolean DEFAULT true NOT NULL,
    health_check_interval integer DEFAULT 15 NOT NULL,
    health_check_timeout integer DEFAULT 5 NOT NULL,
    health_check_retries integer DEFAULT 5 NOT NULL,
    health_check_start_period integer DEFAULT 5 NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    last_online_at timestamp(0) without time zone DEFAULT '2026-11-13 08:05:20'::timestamp without time zone NOT NULL,
    redact_all_env_values_in_logs boolean DEFAULT true NOT NULL,
    include_coolify_default_variables boolean DEFAULT true NOT NULL
);


--
-- Name: standalone_sqlites_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.standalone_sqlites_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: standalone_sqlites_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.standalone_sqlites_id_seq OWNED BY public.standalone_sqlites.id;


--
-- Name: stars; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.stars (
    id bigint NOT NULL,
    user_id bigint NOT NULL,
    starrable_type character varying(255) NOT NULL,
    starrable_id bigint NOT NULL,
    "position" integer DEFAULT 0 NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone
);


--
-- Name: stars_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.stars_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: stars_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.stars_id_seq OWNED BY public.stars.id;


--
-- Name: subscriptions; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.subscriptions (
    id bigint NOT NULL,
    team_id bigint NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    stripe_invoice_paid boolean DEFAULT false NOT NULL,
    stripe_subscription_id character varying(255),
    stripe_customer_id character varying(255),
    stripe_cancel_at_period_end boolean DEFAULT false NOT NULL,
    stripe_plan_id character varying(255),
    stripe_feedback character varying(255),
    stripe_comment text,
    stripe_trial_already_ended boolean DEFAULT false NOT NULL,
    stripe_past_due boolean DEFAULT false NOT NULL,
    stripe_refunded_at timestamp(0) without time zone,
    ended_at timestamp(0) without time zone,
    stripe_checkout_reminder_sent_at timestamp(0) without time zone
);


--
-- Name: subscriptions_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.subscriptions_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: subscriptions_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.subscriptions_id_seq OWNED BY public.subscriptions.id;


--
-- Name: swarm_dockers; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.swarm_dockers (
    id bigint NOT NULL,
    name character varying(255) NOT NULL,
    uuid character varying(255) NOT NULL,
    server_id bigint NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    network character varying(255) NOT NULL
);


--
-- Name: swarm_dockers_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.swarm_dockers_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: swarm_dockers_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.swarm_dockers_id_seq OWNED BY public.swarm_dockers.id;


--
-- Name: taggables; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.taggables (
    tag_id bigint NOT NULL,
    taggable_id bigint NOT NULL,
    taggable_type character varying(255) NOT NULL
);


--
-- Name: tags; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.tags (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    name character varying(255) NOT NULL,
    team_id bigint,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone
);


--
-- Name: tags_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.tags_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: tags_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.tags_id_seq OWNED BY public.tags.id;


--
-- Name: team_invitations; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.team_invitations (
    id bigint NOT NULL,
    uuid character varying(255) NOT NULL,
    team_id bigint NOT NULL,
    email character varying(255) NOT NULL,
    role character varying(255) DEFAULT 'member'::character varying NOT NULL,
    link text NOT NULL,
    via character varying(255) DEFAULT 'link'::character varying NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    access json
);


--
-- Name: team_invitations_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.team_invitations_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: team_invitations_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.team_invitations_id_seq OWNED BY public.team_invitations.id;


--
-- Name: team_user; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.team_user (
    id bigint NOT NULL,
    team_id bigint NOT NULL,
    user_id bigint NOT NULL,
    role character varying(255) DEFAULT 'member'::character varying NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    access json
);


--
-- Name: team_user_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.team_user_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: team_user_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.team_user_id_seq OWNED BY public.team_user.id;


--
-- Name: teams; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.teams (
    id bigint NOT NULL,
    name character varying(255) NOT NULL,
    description character varying(255),
    personal_team boolean DEFAULT false NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    show_boarding boolean DEFAULT false NOT NULL,
    custom_server_limit integer,
    is_mcp_server_enabled boolean DEFAULT true NOT NULL,
    is_build_server_fallback_enabled boolean DEFAULT true NOT NULL,
    avatar_path character varying(255),
    avatar_storage_type character varying(255),
    avatar_s3_storage_id bigint,
    is_two_factor_required boolean DEFAULT false NOT NULL,
    scan_images_on_new_servers boolean DEFAULT false NOT NULL
);


--
-- Name: teams_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.teams_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: teams_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.teams_id_seq OWNED BY public.teams.id;


--
-- Name: telegram_notification_settings; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.telegram_notification_settings (
    id bigint NOT NULL,
    team_id bigint NOT NULL,
    telegram_enabled boolean DEFAULT false NOT NULL,
    telegram_token text,
    telegram_chat_id text,
    deployment_success_telegram_notifications boolean DEFAULT false NOT NULL,
    deployment_failure_telegram_notifications boolean DEFAULT true NOT NULL,
    status_change_telegram_notifications boolean DEFAULT false NOT NULL,
    backup_success_telegram_notifications boolean DEFAULT false NOT NULL,
    backup_failure_telegram_notifications boolean DEFAULT true NOT NULL,
    scheduled_task_success_telegram_notifications boolean DEFAULT false NOT NULL,
    scheduled_task_failure_telegram_notifications boolean DEFAULT true NOT NULL,
    docker_cleanup_success_telegram_notifications boolean DEFAULT false NOT NULL,
    docker_cleanup_failure_telegram_notifications boolean DEFAULT true NOT NULL,
    server_disk_usage_telegram_notifications boolean DEFAULT true NOT NULL,
    server_reachable_telegram_notifications boolean DEFAULT false NOT NULL,
    server_unreachable_telegram_notifications boolean DEFAULT true NOT NULL,
    telegram_notifications_deployment_success_thread_id text,
    telegram_notifications_deployment_failure_thread_id text,
    telegram_notifications_status_change_thread_id text,
    telegram_notifications_backup_success_thread_id text,
    telegram_notifications_backup_failure_thread_id text,
    telegram_notifications_scheduled_task_success_thread_id text,
    telegram_notifications_scheduled_task_failure_thread_id text,
    telegram_notifications_docker_cleanup_success_thread_id text,
    telegram_notifications_docker_cleanup_failure_thread_id text,
    telegram_notifications_server_disk_usage_thread_id text,
    telegram_notifications_server_reachable_thread_id text,
    telegram_notifications_server_unreachable_thread_id text,
    server_patch_telegram_notifications boolean DEFAULT true NOT NULL,
    telegram_notifications_server_patch_thread_id character varying(255),
    telegram_notifications_traefik_outdated_thread_id text,
    traefik_outdated_telegram_notifications boolean DEFAULT true NOT NULL,
    restart_limit_reached_telegram_notifications boolean DEFAULT true NOT NULL,
    telegram_notifications_restart_limit_reached_thread_id text,
    failure_count integer DEFAULT 0 NOT NULL,
    image_vulnerabilities_telegram_notifications boolean DEFAULT false NOT NULL,
    telegram_notifications_image_vulnerabilities_thread_id text
);


--
-- Name: telegram_notification_settings_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.telegram_notification_settings_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: telegram_notification_settings_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.telegram_notification_settings_id_seq OWNED BY public.telegram_notification_settings.id;


--
-- Name: user_changelog_reads; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.user_changelog_reads (
    id bigint NOT NULL,
    user_id bigint NOT NULL,
    release_tag character varying(255) NOT NULL,
    read_at timestamp(0) without time zone NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone
);


--
-- Name: user_changelog_reads_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.user_changelog_reads_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: user_changelog_reads_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.user_changelog_reads_id_seq OWNED BY public.user_changelog_reads.id;


--
-- Name: users; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.users (
    id bigint NOT NULL,
    name character varying(255) DEFAULT 'Anonymous'::character varying NOT NULL,
    email character varying(255) NOT NULL,
    email_verified_at timestamp(0) without time zone,
    password character varying(255),
    remember_token character varying(100),
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    two_factor_secret text,
    two_factor_recovery_codes text,
    two_factor_confirmed_at timestamp(0) without time zone,
    force_password_reset boolean DEFAULT false NOT NULL,
    marketing_emails boolean DEFAULT true NOT NULL,
    pending_email character varying(255),
    email_change_code character varying(6),
    email_change_code_expires_at timestamp(0) without time zone,
    avatar_path character varying(255),
    avatar_storage_type character varying(255),
    avatar_s3_storage_id bigint,
    current_team_id bigint,
    created_before_oauth_identities boolean DEFAULT false NOT NULL
);


--
-- Name: users_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.users_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: users_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.users_id_seq OWNED BY public.users.id;


--
-- Name: volume_backup_s3_replicas; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.volume_backup_s3_replicas (
    id bigint NOT NULL,
    execution_id bigint NOT NULL,
    s3_storage_id bigint,
    s3_uploaded boolean,
    s3_storage_deleted boolean DEFAULT false NOT NULL,
    message text,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone
);


--
-- Name: volume_backup_s3_replicas_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.volume_backup_s3_replicas_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: volume_backup_s3_replicas_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.volume_backup_s3_replicas_id_seq OWNED BY public.volume_backup_s3_replicas.id;


--
-- Name: webhook_notification_settings; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.webhook_notification_settings (
    id bigint NOT NULL,
    team_id bigint NOT NULL,
    webhook_enabled boolean DEFAULT false NOT NULL,
    webhook_url text,
    deployment_success_webhook_notifications boolean DEFAULT false NOT NULL,
    deployment_failure_webhook_notifications boolean DEFAULT true NOT NULL,
    status_change_webhook_notifications boolean DEFAULT false NOT NULL,
    backup_success_webhook_notifications boolean DEFAULT false NOT NULL,
    backup_failure_webhook_notifications boolean DEFAULT true NOT NULL,
    scheduled_task_success_webhook_notifications boolean DEFAULT false NOT NULL,
    scheduled_task_failure_webhook_notifications boolean DEFAULT true NOT NULL,
    docker_cleanup_success_webhook_notifications boolean DEFAULT false NOT NULL,
    docker_cleanup_failure_webhook_notifications boolean DEFAULT true NOT NULL,
    server_disk_usage_webhook_notifications boolean DEFAULT true NOT NULL,
    server_reachable_webhook_notifications boolean DEFAULT false NOT NULL,
    server_unreachable_webhook_notifications boolean DEFAULT true NOT NULL,
    server_patch_webhook_notifications boolean DEFAULT false NOT NULL,
    traefik_outdated_webhook_notifications boolean DEFAULT true NOT NULL,
    restart_limit_reached_webhook_notifications boolean DEFAULT true NOT NULL,
    failure_count integer DEFAULT 0 NOT NULL,
    image_vulnerabilities_webhook_notifications boolean DEFAULT false NOT NULL
);


--
-- Name: webhook_notification_settings_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.webhook_notification_settings_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: webhook_notification_settings_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.webhook_notification_settings_id_seq OWNED BY public.webhook_notification_settings.id;


--
-- Name: activity_log id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.activity_log ALTER COLUMN id SET DEFAULT nextval('public.activity_log_id_seq'::regclass);


--
-- Name: additional_destinations id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.additional_destinations ALTER COLUMN id SET DEFAULT nextval('public.additional_destinations_id_seq'::regclass);


--
-- Name: application_deployment_queues id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.application_deployment_queues ALTER COLUMN id SET DEFAULT nextval('public.application_deployment_queues_id_seq'::regclass);


--
-- Name: application_previews id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.application_previews ALTER COLUMN id SET DEFAULT nextval('public.application_previews_id_seq'::regclass);


--
-- Name: application_settings id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.application_settings ALTER COLUMN id SET DEFAULT nextval('public.application_settings_id_seq'::regclass);


--
-- Name: applications id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.applications ALTER COLUMN id SET DEFAULT nextval('public.applications_id_seq'::regclass);


--
-- Name: audit_events id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.audit_events ALTER COLUMN id SET DEFAULT nextval('public.audit_events_id_seq'::regclass);


--
-- Name: cloud_init_scripts id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cloud_init_scripts ALTER COLUMN id SET DEFAULT nextval('public.cloud_init_scripts_id_seq'::regclass);


--
-- Name: cloud_provider_tokens id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cloud_provider_tokens ALTER COLUMN id SET DEFAULT nextval('public.cloud_provider_tokens_id_seq'::regclass);


--
-- Name: database_backup_s3_replicas id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.database_backup_s3_replicas ALTER COLUMN id SET DEFAULT nextval('public.database_backup_s3_replicas_id_seq'::regclass);


--
-- Name: discord_notification_settings id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.discord_notification_settings ALTER COLUMN id SET DEFAULT nextval('public.discord_notification_settings_id_seq'::regclass);


--
-- Name: dns_provider_zones id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.dns_provider_zones ALTER COLUMN id SET DEFAULT nextval('public.dns_provider_zones_id_seq'::regclass);


--
-- Name: docker_cleanup_executions id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.docker_cleanup_executions ALTER COLUMN id SET DEFAULT nextval('public.docker_cleanup_executions_id_seq'::regclass);


--
-- Name: email_notification_settings id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.email_notification_settings ALTER COLUMN id SET DEFAULT nextval('public.email_notification_settings_id_seq'::regclass);


--
-- Name: environment_variables id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.environment_variables ALTER COLUMN id SET DEFAULT nextval('public.environment_variables_id_seq'::regclass);


--
-- Name: environments id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.environments ALTER COLUMN id SET DEFAULT nextval('public.environments_id_seq'::regclass);


--
-- Name: failed_jobs id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.failed_jobs ALTER COLUMN id SET DEFAULT nextval('public.failed_jobs_id_seq'::regclass);


--
-- Name: gitea_apps id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.gitea_apps ALTER COLUMN id SET DEFAULT nextval('public.gitea_apps_id_seq'::regclass);


--
-- Name: github_apps id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.github_apps ALTER COLUMN id SET DEFAULT nextval('public.github_apps_id_seq'::regclass);


--
-- Name: github_runner_configs id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.github_runner_configs ALTER COLUMN id SET DEFAULT nextval('public.github_runner_configs_id_seq'::regclass);


--
-- Name: github_runner_executions id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.github_runner_executions ALTER COLUMN id SET DEFAULT nextval('public.github_runner_executions_id_seq'::regclass);


--
-- Name: gitlab_apps id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.gitlab_apps ALTER COLUMN id SET DEFAULT nextval('public.gitlab_apps_id_seq'::regclass);


--
-- Name: image_sboms id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.image_sboms ALTER COLUMN id SET DEFAULT nextval('public.image_sboms_id_seq'::regclass);


--
-- Name: image_scan_ignores id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.image_scan_ignores ALTER COLUMN id SET DEFAULT nextval('public.image_scan_ignores_id_seq'::regclass);


--
-- Name: image_scan_notification_states id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.image_scan_notification_states ALTER COLUMN id SET DEFAULT nextval('public.image_scan_notification_states_id_seq'::regclass);


--
-- Name: image_scan_policies id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.image_scan_policies ALTER COLUMN id SET DEFAULT nextval('public.image_scan_policies_id_seq'::regclass);


--
-- Name: image_scans id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.image_scans ALTER COLUMN id SET DEFAULT nextval('public.image_scans_id_seq'::regclass);


--
-- Name: instance_settings id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.instance_settings ALTER COLUMN id SET DEFAULT nextval('public.instance_settings_id_seq'::regclass);


--
-- Name: integration_tokens id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.integration_tokens ALTER COLUMN id SET DEFAULT nextval('public.integration_tokens_id_seq'::regclass);


--
-- Name: local_file_volumes id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.local_file_volumes ALTER COLUMN id SET DEFAULT nextval('public.local_file_volumes_id_seq'::regclass);


--
-- Name: local_persistent_volumes id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.local_persistent_volumes ALTER COLUMN id SET DEFAULT nextval('public.local_persistent_volumes_id_seq'::regclass);


--
-- Name: managed_dns_record_references id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.managed_dns_record_references ALTER COLUMN id SET DEFAULT nextval('public.managed_dns_record_references_id_seq'::regclass);


--
-- Name: managed_dns_records id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.managed_dns_records ALTER COLUMN id SET DEFAULT nextval('public.managed_dns_records_id_seq'::regclass);


--
-- Name: migrations id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.migrations ALTER COLUMN id SET DEFAULT nextval('public.migrations_id_seq'::regclass);


--
-- Name: notification_throttles id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.notification_throttles ALTER COLUMN id SET DEFAULT nextval('public.notification_throttles_id_seq'::regclass);


--
-- Name: oauth_identities id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.oauth_identities ALTER COLUMN id SET DEFAULT nextval('public.oauth_identities_id_seq'::regclass);


--
-- Name: oauth_settings id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.oauth_settings ALTER COLUMN id SET DEFAULT nextval('public.oauth_settings_id_seq'::regclass);


--
-- Name: passkeys id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.passkeys ALTER COLUMN id SET DEFAULT nextval('public.passkeys_id_seq'::regclass);


--
-- Name: personal_access_tokens id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.personal_access_tokens ALTER COLUMN id SET DEFAULT nextval('public.personal_access_tokens_id_seq'::regclass);


--
-- Name: private_keys id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.private_keys ALTER COLUMN id SET DEFAULT nextval('public.private_keys_id_seq'::regclass);


--
-- Name: project_settings id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.project_settings ALTER COLUMN id SET DEFAULT nextval('public.project_settings_id_seq'::regclass);


--
-- Name: projects id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.projects ALTER COLUMN id SET DEFAULT nextval('public.projects_id_seq'::regclass);


--
-- Name: pushover_notification_settings id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.pushover_notification_settings ALTER COLUMN id SET DEFAULT nextval('public.pushover_notification_settings_id_seq'::regclass);


--
-- Name: resource_links id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.resource_links ALTER COLUMN id SET DEFAULT nextval('public.resource_links_id_seq'::regclass);


--
-- Name: s3_storages id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.s3_storages ALTER COLUMN id SET DEFAULT nextval('public.s3_storages_id_seq'::regclass);


--
-- Name: scheduled_database_backup_executions id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_database_backup_executions ALTER COLUMN id SET DEFAULT nextval('public.scheduled_database_backup_executions_id_seq'::regclass);


--
-- Name: scheduled_database_backup_s3_storage id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_database_backup_s3_storage ALTER COLUMN id SET DEFAULT nextval('public.scheduled_database_backup_s3_storage_id_seq'::regclass);


--
-- Name: scheduled_database_backups id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_database_backups ALTER COLUMN id SET DEFAULT nextval('public.scheduled_database_backups_id_seq'::regclass);


--
-- Name: scheduled_job_deliveries id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_job_deliveries ALTER COLUMN id SET DEFAULT nextval('public.scheduled_job_deliveries_id_seq'::regclass);


--
-- Name: scheduled_job_states id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_job_states ALTER COLUMN id SET DEFAULT nextval('public.scheduled_job_states_id_seq'::regclass);


--
-- Name: scheduled_task_executions id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_task_executions ALTER COLUMN id SET DEFAULT nextval('public.scheduled_task_executions_id_seq'::regclass);


--
-- Name: scheduled_tasks id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_tasks ALTER COLUMN id SET DEFAULT nextval('public.scheduled_tasks_id_seq'::regclass);


--
-- Name: scheduled_volume_backup_executions id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_volume_backup_executions ALTER COLUMN id SET DEFAULT nextval('public.scheduled_volume_backup_executions_id_seq'::regclass);


--
-- Name: scheduled_volume_backup_s3_storage id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_volume_backup_s3_storage ALTER COLUMN id SET DEFAULT nextval('public.scheduled_volume_backup_s3_storage_id_seq'::regclass);


--
-- Name: scheduled_volume_backups id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_volume_backups ALTER COLUMN id SET DEFAULT nextval('public.scheduled_volume_backups_id_seq'::regclass);


--
-- Name: secret_manager_links id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.secret_manager_links ALTER COLUMN id SET DEFAULT nextval('public.secret_manager_links_id_seq'::regclass);


--
-- Name: server_host_scans id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.server_host_scans ALTER COLUMN id SET DEFAULT nextval('public.server_host_scans_id_seq'::regclass);


--
-- Name: server_settings id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.server_settings ALTER COLUMN id SET DEFAULT nextval('public.server_settings_id_seq'::regclass);


--
-- Name: servers id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.servers ALTER COLUMN id SET DEFAULT nextval('public.servers_id_seq'::regclass);


--
-- Name: service_applications id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.service_applications ALTER COLUMN id SET DEFAULT nextval('public.service_applications_id_seq'::regclass);


--
-- Name: service_databases id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.service_databases ALTER COLUMN id SET DEFAULT nextval('public.service_databases_id_seq'::regclass);


--
-- Name: services id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.services ALTER COLUMN id SET DEFAULT nextval('public.services_id_seq'::regclass);


--
-- Name: shared_environment_variables id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.shared_environment_variables ALTER COLUMN id SET DEFAULT nextval('public.shared_environment_variables_id_seq'::regclass);


--
-- Name: slack_notification_settings id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.slack_notification_settings ALTER COLUMN id SET DEFAULT nextval('public.slack_notification_settings_id_seq'::regclass);


--
-- Name: ssl_certificates id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ssl_certificates ALTER COLUMN id SET DEFAULT nextval('public.ssl_certificates_id_seq'::regclass);


--
-- Name: standalone_clickhouses id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.standalone_clickhouses ALTER COLUMN id SET DEFAULT nextval('public.standalone_clickhouses_id_seq'::regclass);


--
-- Name: standalone_dockers id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.standalone_dockers ALTER COLUMN id SET DEFAULT nextval('public.standalone_dockers_id_seq'::regclass);


--
-- Name: standalone_dragonflies id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.standalone_dragonflies ALTER COLUMN id SET DEFAULT nextval('public.standalone_dragonflies_id_seq'::regclass);


--
-- Name: standalone_keydbs id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.standalone_keydbs ALTER COLUMN id SET DEFAULT nextval('public.standalone_keydbs_id_seq'::regclass);


--
-- Name: standalone_mariadbs id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.standalone_mariadbs ALTER COLUMN id SET DEFAULT nextval('public.standalone_mariadbs_id_seq'::regclass);


--
-- Name: standalone_mongodbs id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.standalone_mongodbs ALTER COLUMN id SET DEFAULT nextval('public.standalone_mongodbs_id_seq'::regclass);


--
-- Name: standalone_mysqls id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.standalone_mysqls ALTER COLUMN id SET DEFAULT nextval('public.standalone_mysqls_id_seq'::regclass);


--
-- Name: standalone_postgresqls id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.standalone_postgresqls ALTER COLUMN id SET DEFAULT nextval('public.standalone_postgresqls_id_seq'::regclass);


--
-- Name: standalone_redis id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.standalone_redis ALTER COLUMN id SET DEFAULT nextval('public.standalone_redis_id_seq'::regclass);


--
-- Name: standalone_sqlites id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.standalone_sqlites ALTER COLUMN id SET DEFAULT nextval('public.standalone_sqlites_id_seq'::regclass);


--
-- Name: stars id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.stars ALTER COLUMN id SET DEFAULT nextval('public.stars_id_seq'::regclass);


--
-- Name: subscriptions id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.subscriptions ALTER COLUMN id SET DEFAULT nextval('public.subscriptions_id_seq'::regclass);


--
-- Name: swarm_dockers id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.swarm_dockers ALTER COLUMN id SET DEFAULT nextval('public.swarm_dockers_id_seq'::regclass);


--
-- Name: tags id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.tags ALTER COLUMN id SET DEFAULT nextval('public.tags_id_seq'::regclass);


--
-- Name: team_invitations id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_invitations ALTER COLUMN id SET DEFAULT nextval('public.team_invitations_id_seq'::regclass);


--
-- Name: team_user id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_user ALTER COLUMN id SET DEFAULT nextval('public.team_user_id_seq'::regclass);


--
-- Name: teams id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.teams ALTER COLUMN id SET DEFAULT nextval('public.teams_id_seq'::regclass);


--
-- Name: telegram_notification_settings id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.telegram_notification_settings ALTER COLUMN id SET DEFAULT nextval('public.telegram_notification_settings_id_seq'::regclass);


--
-- Name: user_changelog_reads id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_changelog_reads ALTER COLUMN id SET DEFAULT nextval('public.user_changelog_reads_id_seq'::regclass);


--
-- Name: users id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.users ALTER COLUMN id SET DEFAULT nextval('public.users_id_seq'::regclass);


--
-- Name: volume_backup_s3_replicas id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.volume_backup_s3_replicas ALTER COLUMN id SET DEFAULT nextval('public.volume_backup_s3_replicas_id_seq'::regclass);


--
-- Name: webhook_notification_settings id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook_notification_settings ALTER COLUMN id SET DEFAULT nextval('public.webhook_notification_settings_id_seq'::regclass);


--
-- Name: activity_log activity_log_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.activity_log
    ADD CONSTRAINT activity_log_pkey PRIMARY KEY (id);


--
-- Name: additional_destinations additional_destinations_application_server_docker_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.additional_destinations
    ADD CONSTRAINT additional_destinations_application_server_docker_unique UNIQUE (application_id, server_id, standalone_docker_id);


--
-- Name: additional_destinations additional_destinations_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.additional_destinations
    ADD CONSTRAINT additional_destinations_pkey PRIMARY KEY (id);


--
-- Name: application_deployment_queues application_deployment_queues_deployment_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.application_deployment_queues
    ADD CONSTRAINT application_deployment_queues_deployment_uuid_unique UNIQUE (deployment_uuid);


--
-- Name: application_deployment_queues application_deployment_queues_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.application_deployment_queues
    ADD CONSTRAINT application_deployment_queues_pkey PRIMARY KEY (id);


--
-- Name: application_previews application_previews_fqdn_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.application_previews
    ADD CONSTRAINT application_previews_fqdn_unique UNIQUE (fqdn);


--
-- Name: application_previews application_previews_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.application_previews
    ADD CONSTRAINT application_previews_pkey PRIMARY KEY (id);


--
-- Name: application_previews application_previews_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.application_previews
    ADD CONSTRAINT application_previews_uuid_unique UNIQUE (uuid);


--
-- Name: application_settings application_settings_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.application_settings
    ADD CONSTRAINT application_settings_pkey PRIMARY KEY (id);


--
-- Name: applications applications_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.applications
    ADD CONSTRAINT applications_pkey PRIMARY KEY (id);


--
-- Name: applications applications_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.applications
    ADD CONSTRAINT applications_uuid_unique UNIQUE (uuid);


--
-- Name: audit_events audit_events_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.audit_events
    ADD CONSTRAINT audit_events_pkey PRIMARY KEY (id);


--
-- Name: cloud_init_scripts cloud_init_scripts_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cloud_init_scripts
    ADD CONSTRAINT cloud_init_scripts_pkey PRIMARY KEY (id);


--
-- Name: cloud_init_scripts cloud_init_scripts_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cloud_init_scripts
    ADD CONSTRAINT cloud_init_scripts_uuid_unique UNIQUE (uuid);


--
-- Name: cloud_provider_tokens cloud_provider_tokens_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cloud_provider_tokens
    ADD CONSTRAINT cloud_provider_tokens_pkey PRIMARY KEY (id);


--
-- Name: cloud_provider_tokens cloud_provider_tokens_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cloud_provider_tokens
    ADD CONSTRAINT cloud_provider_tokens_uuid_unique UNIQUE (uuid);


--
-- Name: database_backup_s3_replicas database_backup_s3_replicas_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.database_backup_s3_replicas
    ADD CONSTRAINT database_backup_s3_replicas_pkey PRIMARY KEY (id);


--
-- Name: scheduled_database_backup_s3_storage db_backup_s3_storage_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_database_backup_s3_storage
    ADD CONSTRAINT db_backup_s3_storage_unique UNIQUE (scheduled_database_backup_id, s3_storage_id);


--
-- Name: discord_notification_settings discord_notification_settings_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.discord_notification_settings
    ADD CONSTRAINT discord_notification_settings_pkey PRIMARY KEY (id);


--
-- Name: discord_notification_settings discord_notification_settings_team_id_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.discord_notification_settings
    ADD CONSTRAINT discord_notification_settings_team_id_unique UNIQUE (team_id);


--
-- Name: dns_provider_zones dns_provider_zones_integration_token_id_provider_zone_id_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.dns_provider_zones
    ADD CONSTRAINT dns_provider_zones_integration_token_id_provider_zone_id_unique UNIQUE (integration_token_id, provider_zone_id);


--
-- Name: dns_provider_zones dns_provider_zones_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.dns_provider_zones
    ADD CONSTRAINT dns_provider_zones_pkey PRIMARY KEY (id);


--
-- Name: dns_provider_zones dns_provider_zones_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.dns_provider_zones
    ADD CONSTRAINT dns_provider_zones_uuid_unique UNIQUE (uuid);


--
-- Name: docker_cleanup_executions docker_cleanup_executions_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.docker_cleanup_executions
    ADD CONSTRAINT docker_cleanup_executions_pkey PRIMARY KEY (id);


--
-- Name: docker_cleanup_executions docker_cleanup_executions_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.docker_cleanup_executions
    ADD CONSTRAINT docker_cleanup_executions_uuid_unique UNIQUE (uuid);


--
-- Name: email_notification_settings email_notification_settings_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.email_notification_settings
    ADD CONSTRAINT email_notification_settings_pkey PRIMARY KEY (id);


--
-- Name: email_notification_settings email_notification_settings_team_id_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.email_notification_settings
    ADD CONSTRAINT email_notification_settings_team_id_unique UNIQUE (team_id);


--
-- Name: environment_variables environment_variables_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.environment_variables
    ADD CONSTRAINT environment_variables_pkey PRIMARY KEY (id);


--
-- Name: environments environments_name_project_id_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.environments
    ADD CONSTRAINT environments_name_project_id_unique UNIQUE (name, project_id);


--
-- Name: environments environments_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.environments
    ADD CONSTRAINT environments_pkey PRIMARY KEY (id);


--
-- Name: environments environments_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.environments
    ADD CONSTRAINT environments_uuid_unique UNIQUE (uuid);


--
-- Name: failed_jobs failed_jobs_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.failed_jobs
    ADD CONSTRAINT failed_jobs_pkey PRIMARY KEY (id);


--
-- Name: failed_jobs failed_jobs_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.failed_jobs
    ADD CONSTRAINT failed_jobs_uuid_unique UNIQUE (uuid);


--
-- Name: gitea_apps gitea_apps_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.gitea_apps
    ADD CONSTRAINT gitea_apps_pkey PRIMARY KEY (id);


--
-- Name: gitea_apps gitea_apps_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.gitea_apps
    ADD CONSTRAINT gitea_apps_uuid_unique UNIQUE (uuid);


--
-- Name: github_apps github_apps_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.github_apps
    ADD CONSTRAINT github_apps_pkey PRIMARY KEY (id);


--
-- Name: github_apps github_apps_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.github_apps
    ADD CONSTRAINT github_apps_uuid_unique UNIQUE (uuid);


--
-- Name: github_runner_configs github_runner_configs_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.github_runner_configs
    ADD CONSTRAINT github_runner_configs_pkey PRIMARY KEY (id);


--
-- Name: github_runner_configs github_runner_configs_server_id_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.github_runner_configs
    ADD CONSTRAINT github_runner_configs_server_id_unique UNIQUE (server_id);


--
-- Name: github_runner_configs github_runner_configs_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.github_runner_configs
    ADD CONSTRAINT github_runner_configs_uuid_unique UNIQUE (uuid);


--
-- Name: github_runner_executions github_runner_executions_github_app_id_trigger_workflow_job_id_; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.github_runner_executions
    ADD CONSTRAINT github_runner_executions_github_app_id_trigger_workflow_job_id_ UNIQUE (github_app_id, trigger_workflow_job_id);


--
-- Name: github_runner_executions github_runner_executions_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.github_runner_executions
    ADD CONSTRAINT github_runner_executions_pkey PRIMARY KEY (id);


--
-- Name: github_runner_executions github_runner_executions_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.github_runner_executions
    ADD CONSTRAINT github_runner_executions_uuid_unique UNIQUE (uuid);


--
-- Name: gitlab_apps gitlab_apps_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.gitlab_apps
    ADD CONSTRAINT gitlab_apps_pkey PRIMARY KEY (id);


--
-- Name: gitlab_apps gitlab_apps_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.gitlab_apps
    ADD CONSTRAINT gitlab_apps_uuid_unique UNIQUE (uuid);


--
-- Name: image_sboms image_sboms_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.image_sboms
    ADD CONSTRAINT image_sboms_pkey PRIMARY KEY (id);


--
-- Name: image_sboms image_sboms_server_id_image_id_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.image_sboms
    ADD CONSTRAINT image_sboms_server_id_image_id_unique UNIQUE (server_id, image_id);


--
-- Name: image_scan_ignores image_scan_ignores_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.image_scan_ignores
    ADD CONSTRAINT image_scan_ignores_pkey PRIMARY KEY (id);


--
-- Name: image_scan_ignores image_scan_ignores_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.image_scan_ignores
    ADD CONSTRAINT image_scan_ignores_uuid_unique UNIQUE (uuid);


--
-- Name: image_scan_notification_states image_scan_notification_states_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.image_scan_notification_states
    ADD CONSTRAINT image_scan_notification_states_pkey PRIMARY KEY (id);


--
-- Name: image_scan_notification_states image_scan_notification_states_resource_type_resource_id_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.image_scan_notification_states
    ADD CONSTRAINT image_scan_notification_states_resource_type_resource_id_unique UNIQUE (resource_type, resource_id);


--
-- Name: image_scan_policies image_scan_policies_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.image_scan_policies
    ADD CONSTRAINT image_scan_policies_pkey PRIMARY KEY (id);


--
-- Name: image_scan_policies image_scan_policies_scope_type_scope_id_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.image_scan_policies
    ADD CONSTRAINT image_scan_policies_scope_type_scope_id_unique UNIQUE (scope_type, scope_id);


--
-- Name: image_scan_policies image_scan_policies_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.image_scan_policies
    ADD CONSTRAINT image_scan_policies_uuid_unique UNIQUE (uuid);


--
-- Name: image_scans image_scans_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.image_scans
    ADD CONSTRAINT image_scans_pkey PRIMARY KEY (id);


--
-- Name: image_scans image_scans_resource_type_resource_id_image_id_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.image_scans
    ADD CONSTRAINT image_scans_resource_type_resource_id_image_id_unique UNIQUE (resource_type, resource_id, image_id);


--
-- Name: image_scans image_scans_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.image_scans
    ADD CONSTRAINT image_scans_uuid_unique UNIQUE (uuid);


--
-- Name: instance_settings instance_settings_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.instance_settings
    ADD CONSTRAINT instance_settings_pkey PRIMARY KEY (id);


--
-- Name: integration_tokens integration_tokens_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.integration_tokens
    ADD CONSTRAINT integration_tokens_pkey PRIMARY KEY (id);


--
-- Name: integration_tokens integration_tokens_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.integration_tokens
    ADD CONSTRAINT integration_tokens_uuid_unique UNIQUE (uuid);


--
-- Name: local_file_volumes local_file_volumes_mount_path_resource_id_resource_type_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.local_file_volumes
    ADD CONSTRAINT local_file_volumes_mount_path_resource_id_resource_type_unique UNIQUE (mount_path, resource_id, resource_type);


--
-- Name: local_file_volumes local_file_volumes_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.local_file_volumes
    ADD CONSTRAINT local_file_volumes_pkey PRIMARY KEY (id);


--
-- Name: local_persistent_volumes local_persistent_volumes_name_resource_id_resource_type_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.local_persistent_volumes
    ADD CONSTRAINT local_persistent_volumes_name_resource_id_resource_type_unique UNIQUE (name, resource_id, resource_type);


--
-- Name: local_persistent_volumes local_persistent_volumes_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.local_persistent_volumes
    ADD CONSTRAINT local_persistent_volumes_pkey PRIMARY KEY (id);


--
-- Name: local_persistent_volumes local_persistent_volumes_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.local_persistent_volumes
    ADD CONSTRAINT local_persistent_volumes_uuid_unique UNIQUE (uuid);


--
-- Name: managed_dns_record_references managed_dns_record_references_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.managed_dns_record_references
    ADD CONSTRAINT managed_dns_record_references_pkey PRIMARY KEY (id);


--
-- Name: managed_dns_record_references managed_dns_record_references_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.managed_dns_record_references
    ADD CONSTRAINT managed_dns_record_references_unique UNIQUE (managed_dns_record_id, resource_type, resource_id);


--
-- Name: managed_dns_records managed_dns_records_dns_provider_zone_id_provider_record_id_uni; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.managed_dns_records
    ADD CONSTRAINT managed_dns_records_dns_provider_zone_id_provider_record_id_uni UNIQUE (dns_provider_zone_id, provider_record_id);


--
-- Name: managed_dns_records managed_dns_records_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.managed_dns_records
    ADD CONSTRAINT managed_dns_records_pkey PRIMARY KEY (id);


--
-- Name: managed_dns_records managed_dns_records_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.managed_dns_records
    ADD CONSTRAINT managed_dns_records_uuid_unique UNIQUE (uuid);


--
-- Name: migrations migrations_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.migrations
    ADD CONSTRAINT migrations_pkey PRIMARY KEY (id);


--
-- Name: notification_throttles notification_throttles_notifiable_type_notifiable_id_notificati; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.notification_throttles
    ADD CONSTRAINT notification_throttles_notifiable_type_notifiable_id_notificati UNIQUE (notifiable_type, notifiable_id, notification);


--
-- Name: notification_throttles notification_throttles_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.notification_throttles
    ADD CONSTRAINT notification_throttles_pkey PRIMARY KEY (id);


--
-- Name: oauth_identities oauth_identities_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.oauth_identities
    ADD CONSTRAINT oauth_identities_pkey PRIMARY KEY (id);


--
-- Name: oauth_identities oauth_identity_provider_issuer_user_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.oauth_identities
    ADD CONSTRAINT oauth_identity_provider_issuer_user_unique UNIQUE (provider, issuer, provider_user_id);


--
-- Name: oauth_settings oauth_settings_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.oauth_settings
    ADD CONSTRAINT oauth_settings_pkey PRIMARY KEY (id);


--
-- Name: oauth_settings oauth_settings_provider_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.oauth_settings
    ADD CONSTRAINT oauth_settings_provider_unique UNIQUE (provider);


--
-- Name: passkeys passkeys_credential_id_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.passkeys
    ADD CONSTRAINT passkeys_credential_id_unique UNIQUE (credential_id);


--
-- Name: passkeys passkeys_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.passkeys
    ADD CONSTRAINT passkeys_pkey PRIMARY KEY (id);


--
-- Name: password_reset_tokens password_reset_tokens_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.password_reset_tokens
    ADD CONSTRAINT password_reset_tokens_pkey PRIMARY KEY (email);


--
-- Name: personal_access_tokens personal_access_tokens_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.personal_access_tokens
    ADD CONSTRAINT personal_access_tokens_pkey PRIMARY KEY (id);


--
-- Name: personal_access_tokens personal_access_tokens_token_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.personal_access_tokens
    ADD CONSTRAINT personal_access_tokens_token_unique UNIQUE (token);


--
-- Name: private_keys private_keys_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.private_keys
    ADD CONSTRAINT private_keys_pkey PRIMARY KEY (id);


--
-- Name: private_keys private_keys_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.private_keys
    ADD CONSTRAINT private_keys_uuid_unique UNIQUE (uuid);


--
-- Name: project_settings project_settings_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.project_settings
    ADD CONSTRAINT project_settings_pkey PRIMARY KEY (id);


--
-- Name: projects projects_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.projects
    ADD CONSTRAINT projects_pkey PRIMARY KEY (id);


--
-- Name: projects projects_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.projects
    ADD CONSTRAINT projects_uuid_unique UNIQUE (uuid);


--
-- Name: pushover_notification_settings pushover_notification_settings_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.pushover_notification_settings
    ADD CONSTRAINT pushover_notification_settings_pkey PRIMARY KEY (id);


--
-- Name: pushover_notification_settings pushover_notification_settings_team_id_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.pushover_notification_settings
    ADD CONSTRAINT pushover_notification_settings_team_id_unique UNIQUE (team_id);


--
-- Name: resource_links resource_links_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.resource_links
    ADD CONSTRAINT resource_links_pkey PRIMARY KEY (id);


--
-- Name: resource_links resource_links_unique_direction; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.resource_links
    ADD CONSTRAINT resource_links_unique_direction UNIQUE (environment_id, source_type, source_id, target_type, target_id);


--
-- Name: resource_links resource_links_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.resource_links
    ADD CONSTRAINT resource_links_uuid_unique UNIQUE (uuid);


--
-- Name: s3_storages s3_storages_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.s3_storages
    ADD CONSTRAINT s3_storages_pkey PRIMARY KEY (id);


--
-- Name: s3_storages s3_storages_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.s3_storages
    ADD CONSTRAINT s3_storages_uuid_unique UNIQUE (uuid);


--
-- Name: scheduled_database_backup_executions scheduled_database_backup_executions_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_database_backup_executions
    ADD CONSTRAINT scheduled_database_backup_executions_pkey PRIMARY KEY (id);


--
-- Name: scheduled_database_backup_executions scheduled_database_backup_executions_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_database_backup_executions
    ADD CONSTRAINT scheduled_database_backup_executions_uuid_unique UNIQUE (uuid);


--
-- Name: scheduled_database_backup_s3_storage scheduled_database_backup_s3_storage_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_database_backup_s3_storage
    ADD CONSTRAINT scheduled_database_backup_s3_storage_pkey PRIMARY KEY (id);


--
-- Name: scheduled_database_backups scheduled_database_backups_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_database_backups
    ADD CONSTRAINT scheduled_database_backups_pkey PRIMARY KEY (id);


--
-- Name: scheduled_database_backups scheduled_database_backups_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_database_backups
    ADD CONSTRAINT scheduled_database_backups_uuid_unique UNIQUE (uuid);


--
-- Name: scheduled_job_deliveries scheduled_job_deliveries_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_job_deliveries
    ADD CONSTRAINT scheduled_job_deliveries_pkey PRIMARY KEY (id);


--
-- Name: scheduled_job_deliveries scheduled_job_deliveries_schedule_key_scheduled_for_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_job_deliveries
    ADD CONSTRAINT scheduled_job_deliveries_schedule_key_scheduled_for_unique UNIQUE (schedule_key, scheduled_for);


--
-- Name: scheduled_job_deliveries scheduled_job_deliveries_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_job_deliveries
    ADD CONSTRAINT scheduled_job_deliveries_uuid_unique UNIQUE (uuid);


--
-- Name: scheduled_job_states scheduled_job_states_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_job_states
    ADD CONSTRAINT scheduled_job_states_pkey PRIMARY KEY (id);


--
-- Name: scheduled_job_states scheduled_job_states_schedule_key_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_job_states
    ADD CONSTRAINT scheduled_job_states_schedule_key_unique UNIQUE (schedule_key);


--
-- Name: scheduled_job_states scheduled_job_states_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_job_states
    ADD CONSTRAINT scheduled_job_states_uuid_unique UNIQUE (uuid);


--
-- Name: scheduled_task_executions scheduled_task_executions_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_task_executions
    ADD CONSTRAINT scheduled_task_executions_pkey PRIMARY KEY (id);


--
-- Name: scheduled_task_executions scheduled_task_executions_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_task_executions
    ADD CONSTRAINT scheduled_task_executions_uuid_unique UNIQUE (uuid);


--
-- Name: scheduled_tasks scheduled_tasks_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_tasks
    ADD CONSTRAINT scheduled_tasks_pkey PRIMARY KEY (id);


--
-- Name: scheduled_tasks scheduled_tasks_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_tasks
    ADD CONSTRAINT scheduled_tasks_uuid_unique UNIQUE (uuid);


--
-- Name: scheduled_volume_backup_executions scheduled_volume_backup_executions_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_volume_backup_executions
    ADD CONSTRAINT scheduled_volume_backup_executions_pkey PRIMARY KEY (id);


--
-- Name: scheduled_volume_backup_executions scheduled_volume_backup_executions_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_volume_backup_executions
    ADD CONSTRAINT scheduled_volume_backup_executions_uuid_unique UNIQUE (uuid);


--
-- Name: scheduled_volume_backup_s3_storage scheduled_volume_backup_s3_storage_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_volume_backup_s3_storage
    ADD CONSTRAINT scheduled_volume_backup_s3_storage_pkey PRIMARY KEY (id);


--
-- Name: scheduled_volume_backups scheduled_volume_backups_backupable_type_backupable_id_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_volume_backups
    ADD CONSTRAINT scheduled_volume_backups_backupable_type_backupable_id_unique UNIQUE (backupable_type, backupable_id);


--
-- Name: scheduled_volume_backups scheduled_volume_backups_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_volume_backups
    ADD CONSTRAINT scheduled_volume_backups_pkey PRIMARY KEY (id);


--
-- Name: scheduled_volume_backups scheduled_volume_backups_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_volume_backups
    ADD CONSTRAINT scheduled_volume_backups_uuid_unique UNIQUE (uuid);


--
-- Name: secret_manager_links secret_manager_links_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.secret_manager_links
    ADD CONSTRAINT secret_manager_links_pkey PRIMARY KEY (id);


--
-- Name: secret_manager_links secret_manager_links_resourceable_type_resourceable_id_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.secret_manager_links
    ADD CONSTRAINT secret_manager_links_resourceable_type_resourceable_id_unique UNIQUE (resourceable_type, resourceable_id);


--
-- Name: secret_manager_links secret_manager_links_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.secret_manager_links
    ADD CONSTRAINT secret_manager_links_uuid_unique UNIQUE (uuid);


--
-- Name: server_host_scans server_host_scans_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.server_host_scans
    ADD CONSTRAINT server_host_scans_pkey PRIMARY KEY (id);


--
-- Name: server_host_scans server_host_scans_server_id_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.server_host_scans
    ADD CONSTRAINT server_host_scans_server_id_unique UNIQUE (server_id);


--
-- Name: server_settings server_settings_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.server_settings
    ADD CONSTRAINT server_settings_pkey PRIMARY KEY (id);


--
-- Name: servers servers_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.servers
    ADD CONSTRAINT servers_pkey PRIMARY KEY (id);


--
-- Name: servers servers_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.servers
    ADD CONSTRAINT servers_uuid_unique UNIQUE (uuid);


--
-- Name: service_applications service_applications_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.service_applications
    ADD CONSTRAINT service_applications_pkey PRIMARY KEY (id);


--
-- Name: service_applications service_applications_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.service_applications
    ADD CONSTRAINT service_applications_uuid_unique UNIQUE (uuid);


--
-- Name: service_databases service_databases_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.service_databases
    ADD CONSTRAINT service_databases_pkey PRIMARY KEY (id);


--
-- Name: service_databases service_databases_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.service_databases
    ADD CONSTRAINT service_databases_uuid_unique UNIQUE (uuid);


--
-- Name: services services_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.services
    ADD CONSTRAINT services_pkey PRIMARY KEY (id);


--
-- Name: services services_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.services
    ADD CONSTRAINT services_uuid_unique UNIQUE (uuid);


--
-- Name: sessions sessions_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.sessions
    ADD CONSTRAINT sessions_pkey PRIMARY KEY (id);


--
-- Name: shared_environment_variables shared_environment_variables_key_environment_id_team_id_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.shared_environment_variables
    ADD CONSTRAINT shared_environment_variables_key_environment_id_team_id_unique UNIQUE (key, environment_id, team_id);


--
-- Name: shared_environment_variables shared_environment_variables_key_project_id_team_id_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.shared_environment_variables
    ADD CONSTRAINT shared_environment_variables_key_project_id_team_id_unique UNIQUE (key, project_id, team_id);


--
-- Name: shared_environment_variables shared_environment_variables_key_server_id_team_id_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.shared_environment_variables
    ADD CONSTRAINT shared_environment_variables_key_server_id_team_id_unique UNIQUE (key, server_id, team_id);


--
-- Name: shared_environment_variables shared_environment_variables_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.shared_environment_variables
    ADD CONSTRAINT shared_environment_variables_pkey PRIMARY KEY (id);


--
-- Name: slack_notification_settings slack_notification_settings_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.slack_notification_settings
    ADD CONSTRAINT slack_notification_settings_pkey PRIMARY KEY (id);


--
-- Name: slack_notification_settings slack_notification_settings_team_id_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.slack_notification_settings
    ADD CONSTRAINT slack_notification_settings_team_id_unique UNIQUE (team_id);


--
-- Name: ssl_certificates ssl_certificates_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ssl_certificates
    ADD CONSTRAINT ssl_certificates_pkey PRIMARY KEY (id);


--
-- Name: standalone_clickhouses standalone_clickhouses_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.standalone_clickhouses
    ADD CONSTRAINT standalone_clickhouses_pkey PRIMARY KEY (id);


--
-- Name: standalone_clickhouses standalone_clickhouses_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.standalone_clickhouses
    ADD CONSTRAINT standalone_clickhouses_uuid_unique UNIQUE (uuid);


--
-- Name: standalone_dockers standalone_dockers_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.standalone_dockers
    ADD CONSTRAINT standalone_dockers_pkey PRIMARY KEY (id);


--
-- Name: standalone_dockers standalone_dockers_server_id_network_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.standalone_dockers
    ADD CONSTRAINT standalone_dockers_server_id_network_unique UNIQUE (server_id, network);


--
-- Name: standalone_dockers standalone_dockers_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.standalone_dockers
    ADD CONSTRAINT standalone_dockers_uuid_unique UNIQUE (uuid);


--
-- Name: standalone_dragonflies standalone_dragonflies_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.standalone_dragonflies
    ADD CONSTRAINT standalone_dragonflies_pkey PRIMARY KEY (id);


--
-- Name: standalone_dragonflies standalone_dragonflies_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.standalone_dragonflies
    ADD CONSTRAINT standalone_dragonflies_uuid_unique UNIQUE (uuid);


--
-- Name: standalone_keydbs standalone_keydbs_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.standalone_keydbs
    ADD CONSTRAINT standalone_keydbs_pkey PRIMARY KEY (id);


--
-- Name: standalone_keydbs standalone_keydbs_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.standalone_keydbs
    ADD CONSTRAINT standalone_keydbs_uuid_unique UNIQUE (uuid);


--
-- Name: standalone_mariadbs standalone_mariadbs_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.standalone_mariadbs
    ADD CONSTRAINT standalone_mariadbs_pkey PRIMARY KEY (id);


--
-- Name: standalone_mariadbs standalone_mariadbs_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.standalone_mariadbs
    ADD CONSTRAINT standalone_mariadbs_uuid_unique UNIQUE (uuid);


--
-- Name: standalone_mongodbs standalone_mongodbs_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.standalone_mongodbs
    ADD CONSTRAINT standalone_mongodbs_pkey PRIMARY KEY (id);


--
-- Name: standalone_mongodbs standalone_mongodbs_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.standalone_mongodbs
    ADD CONSTRAINT standalone_mongodbs_uuid_unique UNIQUE (uuid);


--
-- Name: standalone_mysqls standalone_mysqls_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.standalone_mysqls
    ADD CONSTRAINT standalone_mysqls_pkey PRIMARY KEY (id);


--
-- Name: standalone_mysqls standalone_mysqls_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.standalone_mysqls
    ADD CONSTRAINT standalone_mysqls_uuid_unique UNIQUE (uuid);


--
-- Name: standalone_postgresqls standalone_postgresqls_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.standalone_postgresqls
    ADD CONSTRAINT standalone_postgresqls_pkey PRIMARY KEY (id);


--
-- Name: standalone_postgresqls standalone_postgresqls_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.standalone_postgresqls
    ADD CONSTRAINT standalone_postgresqls_uuid_unique UNIQUE (uuid);


--
-- Name: standalone_redis standalone_redis_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.standalone_redis
    ADD CONSTRAINT standalone_redis_pkey PRIMARY KEY (id);


--
-- Name: standalone_redis standalone_redis_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.standalone_redis
    ADD CONSTRAINT standalone_redis_uuid_unique UNIQUE (uuid);


--
-- Name: standalone_sqlites standalone_sqlites_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.standalone_sqlites
    ADD CONSTRAINT standalone_sqlites_pkey PRIMARY KEY (id);


--
-- Name: standalone_sqlites standalone_sqlites_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.standalone_sqlites
    ADD CONSTRAINT standalone_sqlites_uuid_unique UNIQUE (uuid);


--
-- Name: stars stars_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.stars
    ADD CONSTRAINT stars_pkey PRIMARY KEY (id);


--
-- Name: stars stars_user_id_starrable_type_starrable_id_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.stars
    ADD CONSTRAINT stars_user_id_starrable_type_starrable_id_unique UNIQUE (user_id, starrable_type, starrable_id);


--
-- Name: subscriptions subscriptions_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.subscriptions
    ADD CONSTRAINT subscriptions_pkey PRIMARY KEY (id);


--
-- Name: swarm_dockers swarm_dockers_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.swarm_dockers
    ADD CONSTRAINT swarm_dockers_pkey PRIMARY KEY (id);


--
-- Name: swarm_dockers swarm_dockers_server_id_network_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.swarm_dockers
    ADD CONSTRAINT swarm_dockers_server_id_network_unique UNIQUE (server_id, network);


--
-- Name: swarm_dockers swarm_dockers_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.swarm_dockers
    ADD CONSTRAINT swarm_dockers_uuid_unique UNIQUE (uuid);


--
-- Name: taggables taggable_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.taggables
    ADD CONSTRAINT taggable_unique UNIQUE (tag_id, taggable_id, taggable_type);


--
-- Name: tags tags_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.tags
    ADD CONSTRAINT tags_pkey PRIMARY KEY (id);


--
-- Name: tags tags_team_id_name_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.tags
    ADD CONSTRAINT tags_team_id_name_unique UNIQUE (team_id, name);


--
-- Name: tags tags_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.tags
    ADD CONSTRAINT tags_uuid_unique UNIQUE (uuid);


--
-- Name: team_invitations team_invitations_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_invitations
    ADD CONSTRAINT team_invitations_pkey PRIMARY KEY (id);


--
-- Name: team_invitations team_invitations_team_id_email_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_invitations
    ADD CONSTRAINT team_invitations_team_id_email_unique UNIQUE (team_id, email);


--
-- Name: team_invitations team_invitations_uuid_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_invitations
    ADD CONSTRAINT team_invitations_uuid_unique UNIQUE (uuid);


--
-- Name: team_user team_user_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_user
    ADD CONSTRAINT team_user_pkey PRIMARY KEY (id);


--
-- Name: team_user team_user_team_id_user_id_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_user
    ADD CONSTRAINT team_user_team_id_user_id_unique UNIQUE (team_id, user_id);


--
-- Name: teams teams_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.teams
    ADD CONSTRAINT teams_pkey PRIMARY KEY (id);


--
-- Name: telegram_notification_settings telegram_notification_settings_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.telegram_notification_settings
    ADD CONSTRAINT telegram_notification_settings_pkey PRIMARY KEY (id);


--
-- Name: telegram_notification_settings telegram_notification_settings_team_id_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.telegram_notification_settings
    ADD CONSTRAINT telegram_notification_settings_team_id_unique UNIQUE (team_id);


--
-- Name: user_changelog_reads user_changelog_reads_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_changelog_reads
    ADD CONSTRAINT user_changelog_reads_pkey PRIMARY KEY (id);


--
-- Name: user_changelog_reads user_changelog_reads_user_id_release_tag_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_changelog_reads
    ADD CONSTRAINT user_changelog_reads_user_id_release_tag_unique UNIQUE (user_id, release_tag);


--
-- Name: users users_email_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.users
    ADD CONSTRAINT users_email_unique UNIQUE (email);


--
-- Name: users users_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.users
    ADD CONSTRAINT users_pkey PRIMARY KEY (id);


--
-- Name: volume_backup_s3_replicas volume_backup_s3_replicas_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.volume_backup_s3_replicas
    ADD CONSTRAINT volume_backup_s3_replicas_pkey PRIMARY KEY (id);


--
-- Name: scheduled_volume_backup_s3_storage volume_backup_s3_storage_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_volume_backup_s3_storage
    ADD CONSTRAINT volume_backup_s3_storage_unique UNIQUE (scheduled_volume_backup_id, s3_storage_id);


--
-- Name: webhook_notification_settings webhook_notification_settings_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook_notification_settings
    ADD CONSTRAINT webhook_notification_settings_pkey PRIMARY KEY (id);


--
-- Name: webhook_notification_settings webhook_notification_settings_team_id_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook_notification_settings
    ADD CONSTRAINT webhook_notification_settings_team_id_unique UNIQUE (team_id);


--
-- Name: activity_log_log_name_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX activity_log_log_name_index ON public.activity_log USING btree (log_name);


--
-- Name: application_deployment_queues_parent_deployment_uuid_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX application_deployment_queues_parent_deployment_uuid_index ON public.application_deployment_queues USING btree (parent_deployment_uuid);


--
-- Name: application_previews_application_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX application_previews_application_id_index ON public.application_previews USING btree (application_id);


--
-- Name: application_settings_application_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX application_settings_application_id_index ON public.application_settings USING btree (application_id);


--
-- Name: applications_destination_type_destination_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX applications_destination_type_destination_id_index ON public.applications USING btree (destination_type, destination_id);


--
-- Name: applications_source_type_source_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX applications_source_type_source_id_index ON public.applications USING btree (source_type, source_id);


--
-- Name: audit_events_created_at_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX audit_events_created_at_index ON public.audit_events USING btree (created_at);


--
-- Name: audit_events_team_id_action_created_at_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX audit_events_team_id_action_created_at_id_index ON public.audit_events USING btree (team_id, action, created_at, id);


--
-- Name: audit_events_team_id_actor_id_created_at_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX audit_events_team_id_actor_id_created_at_index ON public.audit_events USING btree (team_id, actor_id, created_at);


--
-- Name: audit_events_team_id_created_at_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX audit_events_team_id_created_at_id_index ON public.audit_events USING btree (team_id, created_at, id);


--
-- Name: audit_events_team_id_source_created_at_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX audit_events_team_id_source_created_at_id_index ON public.audit_events USING btree (team_id, source, created_at, id);


--
-- Name: causer; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX causer ON public.activity_log USING btree (causer_type, causer_id);


--
-- Name: cloud_provider_tokens_team_id_provider_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX cloud_provider_tokens_team_id_provider_index ON public.cloud_provider_tokens USING btree (team_id, provider);


--
-- Name: database_backup_s3_replicas_execution_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX database_backup_s3_replicas_execution_id_index ON public.database_backup_s3_replicas USING btree (execution_id);


--
-- Name: dns_provider_zones_name_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX dns_provider_zones_name_index ON public.dns_provider_zones USING btree (name);


--
-- Name: environment_variables_resourceable_type_resourceable_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX environment_variables_resourceable_type_resourceable_id_index ON public.environment_variables USING btree (resourceable_type, resourceable_id);


--
-- Name: github_runner_executions_github_app_id_runner_name_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX github_runner_executions_github_app_id_runner_name_index ON public.github_runner_executions USING btree (github_app_id, runner_name);


--
-- Name: github_runner_executions_github_app_id_status_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX github_runner_executions_github_app_id_status_index ON public.github_runner_executions USING btree (github_app_id, status);


--
-- Name: github_runner_executions_github_runner_config_id_status_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX github_runner_executions_github_runner_config_id_status_index ON public.github_runner_executions USING btree (github_runner_config_id, status);


--
-- Name: idx_activity_properties_status; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_activity_properties_status ON public.activity_log USING btree (((properties ->> 'status'::text)));


--
-- Name: idx_activity_type_uuid; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_activity_type_uuid ON public.activity_log USING gin (properties jsonb_path_ops);


--
-- Name: idx_activity_type_uuid_created_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_activity_type_uuid_created_at ON public.activity_log USING btree (((properties ->> 'type_uuid'::text)), created_at DESC);


--
-- Name: idx_applications_environment_id_all; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_applications_environment_id_all ON public.applications USING btree (environment_id);


--
-- Name: idx_cloud_init_scripts_team_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_cloud_init_scripts_team_id ON public.cloud_init_scripts USING btree (team_id);


--
-- Name: idx_cloud_provider_tokens_team_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_cloud_provider_tokens_team_id ON public.cloud_provider_tokens USING btree (team_id);


--
-- Name: idx_deployment_queues_app_created_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_deployment_queues_app_created_id ON public.application_deployment_queues USING btree (application_id, created_at DESC, id DESC);


--
-- Name: idx_deployment_queues_app_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_deployment_queues_app_id ON public.application_deployment_queues USING btree (application_id, id);


--
-- Name: idx_deployment_queues_app_status_pr_created; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_deployment_queues_app_status_pr_created ON public.application_deployment_queues USING btree (application_id, status, pull_request_id, created_at);


--
-- Name: idx_deployment_queues_status_server; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_deployment_queues_status_server ON public.application_deployment_queues USING btree (status, server_id);


--
-- Name: idx_docker_cleanup_executions_server_created_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_docker_cleanup_executions_server_created_at ON public.docker_cleanup_executions USING btree (server_id, created_at DESC);


--
-- Name: idx_docker_cleanup_executions_unfinished_status_created; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_docker_cleanup_executions_unfinished_status_created ON public.docker_cleanup_executions USING btree (status, created_at) WHERE (finished_at IS NULL);


--
-- Name: idx_environments_project_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_environments_project_id ON public.environments USING btree (project_id);


--
-- Name: idx_github_runner_executions_status_server_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_github_runner_executions_status_server_id ON public.github_runner_executions USING btree (status, server_id);


--
-- Name: idx_private_keys_team_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_private_keys_team_id ON public.private_keys USING btree (team_id);


--
-- Name: idx_projects_team_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_projects_team_id ON public.projects USING btree (team_id);


--
-- Name: idx_server_settings_server_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_server_settings_server_id ON public.server_settings USING btree (server_id);


--
-- Name: idx_servers_private_key_id_active; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_servers_private_key_id_active ON public.servers USING btree (private_key_id) WHERE (deleted_at IS NULL);


--
-- Name: idx_servers_team_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_servers_team_id ON public.servers USING btree (team_id);


--
-- Name: idx_subscriptions_team_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_subscriptions_team_id ON public.subscriptions USING btree (team_id);


--
-- Name: idx_team_user_user_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_team_user_user_id ON public.team_user USING btree (user_id);


--
-- Name: image_scan_ignores_resource_type_resource_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX image_scan_ignores_resource_type_resource_id_index ON public.image_scan_ignores USING btree (resource_type, resource_id);


--
-- Name: image_scan_ignores_team_id_resource_type_resource_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX image_scan_ignores_team_id_resource_type_resource_id_index ON public.image_scan_ignores USING btree (team_id, resource_type, resource_id);


--
-- Name: image_scans_resource_type_resource_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX image_scans_resource_type_resource_id_index ON public.image_scans USING btree (resource_type, resource_id);


--
-- Name: image_scans_server_id_image_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX image_scans_server_id_image_id_index ON public.image_scans USING btree (server_id, image_id);


--
-- Name: integration_tokens_team_id_provider_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX integration_tokens_team_id_provider_index ON public.integration_tokens USING btree (team_id, provider);


--
-- Name: local_file_volumes_resource_type_resource_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX local_file_volumes_resource_type_resource_id_index ON public.local_file_volumes USING btree (resource_type, resource_id);


--
-- Name: local_persistent_volumes_resource_type_resource_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX local_persistent_volumes_resource_type_resource_id_index ON public.local_persistent_volumes USING btree (resource_type, resource_id);


--
-- Name: managed_dns_record_references_resource_type_resource_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX managed_dns_record_references_resource_type_resource_id_index ON public.managed_dns_record_references USING btree (resource_type, resource_id);


--
-- Name: managed_dns_records_team_id_name_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX managed_dns_records_team_id_name_index ON public.managed_dns_records USING btree (team_id, name);


--
-- Name: notification_throttles_notifiable_type_notifiable_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX notification_throttles_notifiable_type_notifiable_id_index ON public.notification_throttles USING btree (notifiable_type, notifiable_id);


--
-- Name: oauth_identities_email_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX oauth_identities_email_index ON public.oauth_identities USING btree (email);


--
-- Name: passkeys_user_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX passkeys_user_id_index ON public.passkeys USING btree (user_id);


--
-- Name: personal_access_tokens_expiration_warning_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX personal_access_tokens_expiration_warning_index ON public.personal_access_tokens USING btree (expires_at, api_token_expiration_warning_sent_at);


--
-- Name: personal_access_tokens_tokenable_type_tokenable_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX personal_access_tokens_tokenable_type_tokenable_id_index ON public.personal_access_tokens USING btree (tokenable_type, tokenable_id);


--
-- Name: resource_links_source_type_source_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX resource_links_source_type_source_id_index ON public.resource_links USING btree (source_type, source_id);


--
-- Name: resource_links_target_type_target_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX resource_links_target_type_target_id_index ON public.resource_links USING btree (target_type, target_id);


--
-- Name: scheduled_database_backups_database_type_database_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX scheduled_database_backups_database_type_database_id_index ON public.scheduled_database_backups USING btree (database_type, database_id);


--
-- Name: scheduled_db_backup_executions_backup_id_created_at_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX scheduled_db_backup_executions_backup_id_created_at_index ON public.scheduled_database_backup_executions USING btree (scheduled_database_backup_id, created_at);


--
-- Name: scheduled_job_deliveries_status_created_at_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX scheduled_job_deliveries_status_created_at_index ON public.scheduled_job_deliveries USING btree (status, created_at);


--
-- Name: scheduled_task_executions_task_id_created_at_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX scheduled_task_executions_task_id_created_at_index ON public.scheduled_task_executions USING btree (scheduled_task_id, created_at);


--
-- Name: scheduled_volume_executions_backup_created_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX scheduled_volume_executions_backup_created_index ON public.scheduled_volume_backup_executions USING btree (scheduled_volume_backup_id, created_at);


--
-- Name: scheduled_volume_executions_recovery_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX scheduled_volume_executions_recovery_index ON public.scheduled_volume_backup_executions USING btree (stop_recovery_pending, s3_cleanup_pending, recovery_needs_attention);


--
-- Name: servers_sentinel_updated_at_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX servers_sentinel_updated_at_index ON public.servers USING btree (sentinel_updated_at);


--
-- Name: service_applications_service_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX service_applications_service_id_index ON public.service_applications USING btree (service_id);


--
-- Name: service_databases_service_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX service_databases_service_id_index ON public.service_databases USING btree (service_id);


--
-- Name: services_destination_type_destination_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX services_destination_type_destination_id_index ON public.services USING btree (destination_type, destination_id);


--
-- Name: services_server_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX services_server_id_index ON public.services USING btree (server_id);


--
-- Name: sessions_last_activity_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX sessions_last_activity_index ON public.sessions USING btree (last_activity);


--
-- Name: sessions_user_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX sessions_user_id_index ON public.sessions USING btree (user_id);


--
-- Name: standalone_clickhouses_destination_type_destination_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX standalone_clickhouses_destination_type_destination_id_index ON public.standalone_clickhouses USING btree (destination_type, destination_id);


--
-- Name: standalone_dragonflies_destination_type_destination_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX standalone_dragonflies_destination_type_destination_id_index ON public.standalone_dragonflies USING btree (destination_type, destination_id);


--
-- Name: standalone_keydbs_destination_type_destination_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX standalone_keydbs_destination_type_destination_id_index ON public.standalone_keydbs USING btree (destination_type, destination_id);


--
-- Name: standalone_mariadbs_destination_type_destination_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX standalone_mariadbs_destination_type_destination_id_index ON public.standalone_mariadbs USING btree (destination_type, destination_id);


--
-- Name: standalone_mongodbs_destination_type_destination_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX standalone_mongodbs_destination_type_destination_id_index ON public.standalone_mongodbs USING btree (destination_type, destination_id);


--
-- Name: standalone_mysqls_destination_type_destination_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX standalone_mysqls_destination_type_destination_id_index ON public.standalone_mysqls USING btree (destination_type, destination_id);


--
-- Name: standalone_postgresqls_destination_type_destination_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX standalone_postgresqls_destination_type_destination_id_index ON public.standalone_postgresqls USING btree (destination_type, destination_id);


--
-- Name: standalone_redis_destination_type_destination_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX standalone_redis_destination_type_destination_id_index ON public.standalone_redis USING btree (destination_type, destination_id);


--
-- Name: standalone_sqlites_destination_type_destination_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX standalone_sqlites_destination_type_destination_id_index ON public.standalone_sqlites USING btree (destination_type, destination_id);


--
-- Name: stars_starrable_type_starrable_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX stars_starrable_type_starrable_id_index ON public.stars USING btree (starrable_type, starrable_id);


--
-- Name: subject; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX subject ON public.activity_log USING btree (subject_type, subject_id);


--
-- Name: subscriptions_ended_at_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX subscriptions_ended_at_index ON public.subscriptions USING btree (ended_at);


--
-- Name: swarm_dockers_server_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX swarm_dockers_server_id_index ON public.swarm_dockers USING btree (server_id);


--
-- Name: user_changelog_reads_release_tag_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX user_changelog_reads_release_tag_index ON public.user_changelog_reads USING btree (release_tag);


--
-- Name: user_changelog_reads_user_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX user_changelog_reads_user_id_index ON public.user_changelog_reads USING btree (user_id);


--
-- Name: volume_backup_s3_replicas_execution_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX volume_backup_s3_replicas_execution_id_index ON public.volume_backup_s3_replicas USING btree (execution_id);


--
-- Name: additional_destinations additional_destinations_application_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.additional_destinations
    ADD CONSTRAINT additional_destinations_application_id_foreign FOREIGN KEY (application_id) REFERENCES public.applications(id) ON DELETE CASCADE;


--
-- Name: additional_destinations additional_destinations_server_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.additional_destinations
    ADD CONSTRAINT additional_destinations_server_id_foreign FOREIGN KEY (server_id) REFERENCES public.servers(id) ON DELETE CASCADE;


--
-- Name: additional_destinations additional_destinations_standalone_docker_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.additional_destinations
    ADD CONSTRAINT additional_destinations_standalone_docker_id_foreign FOREIGN KEY (standalone_docker_id) REFERENCES public.standalone_dockers(id) ON DELETE CASCADE;


--
-- Name: application_deployment_queues application_deployment_queues_scan_gate_bypassed_by_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.application_deployment_queues
    ADD CONSTRAINT application_deployment_queues_scan_gate_bypassed_by_foreign FOREIGN KEY (scan_gate_bypassed_by) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: cloud_init_scripts cloud_init_scripts_team_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cloud_init_scripts
    ADD CONSTRAINT cloud_init_scripts_team_id_foreign FOREIGN KEY (team_id) REFERENCES public.teams(id) ON DELETE CASCADE;


--
-- Name: cloud_provider_tokens cloud_provider_tokens_team_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cloud_provider_tokens
    ADD CONSTRAINT cloud_provider_tokens_team_id_foreign FOREIGN KEY (team_id) REFERENCES public.teams(id) ON DELETE CASCADE;


--
-- Name: database_backup_s3_replicas db_backup_s3_replicas_execution_fk; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.database_backup_s3_replicas
    ADD CONSTRAINT db_backup_s3_replicas_execution_fk FOREIGN KEY (execution_id) REFERENCES public.scheduled_database_backup_executions(id) ON DELETE CASCADE;


--
-- Name: database_backup_s3_replicas db_backup_s3_replicas_storage_fk; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.database_backup_s3_replicas
    ADD CONSTRAINT db_backup_s3_replicas_storage_fk FOREIGN KEY (s3_storage_id) REFERENCES public.s3_storages(id) ON DELETE SET NULL;


--
-- Name: scheduled_database_backup_s3_storage db_backup_s3_storage_backup_fk; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_database_backup_s3_storage
    ADD CONSTRAINT db_backup_s3_storage_backup_fk FOREIGN KEY (scheduled_database_backup_id) REFERENCES public.scheduled_database_backups(id) ON DELETE CASCADE;


--
-- Name: scheduled_database_backup_s3_storage db_backup_s3_storage_storage_fk; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_database_backup_s3_storage
    ADD CONSTRAINT db_backup_s3_storage_storage_fk FOREIGN KEY (s3_storage_id) REFERENCES public.s3_storages(id) ON DELETE CASCADE;


--
-- Name: discord_notification_settings discord_notification_settings_team_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.discord_notification_settings
    ADD CONSTRAINT discord_notification_settings_team_id_foreign FOREIGN KEY (team_id) REFERENCES public.teams(id) ON DELETE CASCADE;


--
-- Name: dns_provider_zones dns_provider_zones_integration_token_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.dns_provider_zones
    ADD CONSTRAINT dns_provider_zones_integration_token_id_foreign FOREIGN KEY (integration_token_id) REFERENCES public.integration_tokens(id) ON DELETE CASCADE;


--
-- Name: email_notification_settings email_notification_settings_team_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.email_notification_settings
    ADD CONSTRAINT email_notification_settings_team_id_foreign FOREIGN KEY (team_id) REFERENCES public.teams(id) ON DELETE CASCADE;


--
-- Name: github_runner_configs github_runner_configs_github_app_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.github_runner_configs
    ADD CONSTRAINT github_runner_configs_github_app_id_foreign FOREIGN KEY (github_app_id) REFERENCES public.github_apps(id) ON DELETE CASCADE;


--
-- Name: github_runner_configs github_runner_configs_server_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.github_runner_configs
    ADD CONSTRAINT github_runner_configs_server_id_foreign FOREIGN KEY (server_id) REFERENCES public.servers(id) ON DELETE CASCADE;


--
-- Name: github_runner_executions github_runner_executions_github_app_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.github_runner_executions
    ADD CONSTRAINT github_runner_executions_github_app_id_foreign FOREIGN KEY (github_app_id) REFERENCES public.github_apps(id) ON DELETE CASCADE;


--
-- Name: github_runner_executions github_runner_executions_github_runner_config_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.github_runner_executions
    ADD CONSTRAINT github_runner_executions_github_runner_config_id_foreign FOREIGN KEY (github_runner_config_id) REFERENCES public.github_runner_configs(id) ON DELETE SET NULL;


--
-- Name: github_runner_executions github_runner_executions_server_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.github_runner_executions
    ADD CONSTRAINT github_runner_executions_server_id_foreign FOREIGN KEY (server_id) REFERENCES public.servers(id) ON DELETE SET NULL;


--
-- Name: image_sboms image_sboms_server_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.image_sboms
    ADD CONSTRAINT image_sboms_server_id_foreign FOREIGN KEY (server_id) REFERENCES public.servers(id) ON DELETE CASCADE;


--
-- Name: image_scan_ignores image_scan_ignores_created_by_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.image_scan_ignores
    ADD CONSTRAINT image_scan_ignores_created_by_foreign FOREIGN KEY (created_by) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: image_scan_ignores image_scan_ignores_team_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.image_scan_ignores
    ADD CONSTRAINT image_scan_ignores_team_id_foreign FOREIGN KEY (team_id) REFERENCES public.teams(id) ON DELETE CASCADE;


--
-- Name: image_scan_notification_states image_scan_notification_states_team_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.image_scan_notification_states
    ADD CONSTRAINT image_scan_notification_states_team_id_foreign FOREIGN KEY (team_id) REFERENCES public.teams(id) ON DELETE CASCADE;


--
-- Name: image_scans image_scans_deployment_queue_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.image_scans
    ADD CONSTRAINT image_scans_deployment_queue_id_foreign FOREIGN KEY (deployment_queue_id) REFERENCES public.application_deployment_queues(id) ON DELETE SET NULL;


--
-- Name: image_scans image_scans_image_sbom_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.image_scans
    ADD CONSTRAINT image_scans_image_sbom_id_foreign FOREIGN KEY (image_sbom_id) REFERENCES public.image_sboms(id) ON DELETE SET NULL;


--
-- Name: image_scans image_scans_server_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.image_scans
    ADD CONSTRAINT image_scans_server_id_foreign FOREIGN KEY (server_id) REFERENCES public.servers(id) ON DELETE CASCADE;


--
-- Name: instance_settings instance_settings_avatar_s3_storage_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.instance_settings
    ADD CONSTRAINT instance_settings_avatar_s3_storage_id_foreign FOREIGN KEY (avatar_s3_storage_id) REFERENCES public.s3_storages(id) ON DELETE SET NULL;


--
-- Name: instance_settings instance_settings_file_download_s3_storage_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.instance_settings
    ADD CONSTRAINT instance_settings_file_download_s3_storage_id_foreign FOREIGN KEY (file_download_s3_storage_id) REFERENCES public.s3_storages(id) ON DELETE SET NULL;


--
-- Name: integration_tokens integration_tokens_team_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.integration_tokens
    ADD CONSTRAINT integration_tokens_team_id_foreign FOREIGN KEY (team_id) REFERENCES public.teams(id) ON DELETE CASCADE;


--
-- Name: local_persistent_volumes local_persistent_volumes_standalone_sqlite_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.local_persistent_volumes
    ADD CONSTRAINT local_persistent_volumes_standalone_sqlite_id_foreign FOREIGN KEY (standalone_sqlite_id) REFERENCES public.standalone_sqlites(id) ON DELETE SET NULL;


--
-- Name: managed_dns_record_references managed_dns_record_references_managed_dns_record_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.managed_dns_record_references
    ADD CONSTRAINT managed_dns_record_references_managed_dns_record_id_foreign FOREIGN KEY (managed_dns_record_id) REFERENCES public.managed_dns_records(id) ON DELETE CASCADE;


--
-- Name: managed_dns_records managed_dns_records_dns_provider_zone_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.managed_dns_records
    ADD CONSTRAINT managed_dns_records_dns_provider_zone_id_foreign FOREIGN KEY (dns_provider_zone_id) REFERENCES public.dns_provider_zones(id) ON DELETE CASCADE;


--
-- Name: managed_dns_records managed_dns_records_integration_token_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.managed_dns_records
    ADD CONSTRAINT managed_dns_records_integration_token_id_foreign FOREIGN KEY (integration_token_id) REFERENCES public.integration_tokens(id) ON DELETE CASCADE;


--
-- Name: managed_dns_records managed_dns_records_team_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.managed_dns_records
    ADD CONSTRAINT managed_dns_records_team_id_foreign FOREIGN KEY (team_id) REFERENCES public.teams(id) ON DELETE CASCADE;


--
-- Name: oauth_identities oauth_identities_user_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.oauth_identities
    ADD CONSTRAINT oauth_identities_user_id_foreign FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: passkeys passkeys_user_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.passkeys
    ADD CONSTRAINT passkeys_user_id_foreign FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: projects projects_icon_s3_storage_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.projects
    ADD CONSTRAINT projects_icon_s3_storage_id_foreign FOREIGN KEY (icon_s3_storage_id) REFERENCES public.s3_storages(id) ON DELETE SET NULL;


--
-- Name: pushover_notification_settings pushover_notification_settings_team_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.pushover_notification_settings
    ADD CONSTRAINT pushover_notification_settings_team_id_foreign FOREIGN KEY (team_id) REFERENCES public.teams(id) ON DELETE CASCADE;


--
-- Name: resource_links resource_links_created_by_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.resource_links
    ADD CONSTRAINT resource_links_created_by_foreign FOREIGN KEY (created_by) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: resource_links resource_links_environment_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.resource_links
    ADD CONSTRAINT resource_links_environment_id_foreign FOREIGN KEY (environment_id) REFERENCES public.environments(id) ON DELETE CASCADE;


--
-- Name: s3_storages s3_storages_private_key_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.s3_storages
    ADD CONSTRAINT s3_storages_private_key_id_foreign FOREIGN KEY (private_key_id) REFERENCES public.private_keys(id) ON DELETE SET NULL;


--
-- Name: scheduled_volume_backup_executions scheduled_volume_backup_executions_s3_storage_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_volume_backup_executions
    ADD CONSTRAINT scheduled_volume_backup_executions_s3_storage_id_foreign FOREIGN KEY (s3_storage_id) REFERENCES public.s3_storages(id) ON DELETE SET NULL;


--
-- Name: scheduled_volume_backup_executions scheduled_volume_backup_executions_scheduled_volume_backup_id_f; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_volume_backup_executions
    ADD CONSTRAINT scheduled_volume_backup_executions_scheduled_volume_backup_id_f FOREIGN KEY (scheduled_volume_backup_id) REFERENCES public.scheduled_volume_backups(id) ON DELETE CASCADE;


--
-- Name: scheduled_volume_backups scheduled_volume_backups_s3_storage_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_volume_backups
    ADD CONSTRAINT scheduled_volume_backups_s3_storage_id_foreign FOREIGN KEY (s3_storage_id) REFERENCES public.s3_storages(id) ON DELETE SET NULL;


--
-- Name: scheduled_volume_backups scheduled_volume_backups_team_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_volume_backups
    ADD CONSTRAINT scheduled_volume_backups_team_id_foreign FOREIGN KEY (team_id) REFERENCES public.teams(id) ON DELETE CASCADE;


--
-- Name: secret_manager_links secret_manager_links_integration_token_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.secret_manager_links
    ADD CONSTRAINT secret_manager_links_integration_token_id_foreign FOREIGN KEY (integration_token_id) REFERENCES public.integration_tokens(id) ON DELETE CASCADE;


--
-- Name: server_host_scans server_host_scans_server_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.server_host_scans
    ADD CONSTRAINT server_host_scans_server_id_foreign FOREIGN KEY (server_id) REFERENCES public.servers(id) ON DELETE CASCADE;


--
-- Name: server_settings server_settings_cloudflare_tunnel_integration_token_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.server_settings
    ADD CONSTRAINT server_settings_cloudflare_tunnel_integration_token_id_foreign FOREIGN KEY (cloudflare_tunnel_integration_token_id) REFERENCES public.integration_tokens(id) ON DELETE SET NULL;


--
-- Name: servers servers_cloud_provider_token_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.servers
    ADD CONSTRAINT servers_cloud_provider_token_id_foreign FOREIGN KEY (cloud_provider_token_id) REFERENCES public.cloud_provider_tokens(id) ON DELETE SET NULL;


--
-- Name: shared_environment_variables shared_environment_variables_environment_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.shared_environment_variables
    ADD CONSTRAINT shared_environment_variables_environment_id_foreign FOREIGN KEY (environment_id) REFERENCES public.environments(id) ON DELETE CASCADE;


--
-- Name: shared_environment_variables shared_environment_variables_project_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.shared_environment_variables
    ADD CONSTRAINT shared_environment_variables_project_id_foreign FOREIGN KEY (project_id) REFERENCES public.projects(id) ON DELETE CASCADE;


--
-- Name: shared_environment_variables shared_environment_variables_server_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.shared_environment_variables
    ADD CONSTRAINT shared_environment_variables_server_id_foreign FOREIGN KEY (server_id) REFERENCES public.servers(id) ON DELETE CASCADE;


--
-- Name: shared_environment_variables shared_environment_variables_team_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.shared_environment_variables
    ADD CONSTRAINT shared_environment_variables_team_id_foreign FOREIGN KEY (team_id) REFERENCES public.teams(id) ON DELETE CASCADE;


--
-- Name: slack_notification_settings slack_notification_settings_team_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.slack_notification_settings
    ADD CONSTRAINT slack_notification_settings_team_id_foreign FOREIGN KEY (team_id) REFERENCES public.teams(id) ON DELETE CASCADE;


--
-- Name: ssl_certificates ssl_certificates_server_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ssl_certificates
    ADD CONSTRAINT ssl_certificates_server_id_foreign FOREIGN KEY (server_id) REFERENCES public.servers(id);


--
-- Name: stars stars_user_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.stars
    ADD CONSTRAINT stars_user_id_foreign FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: taggables taggables_tag_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.taggables
    ADD CONSTRAINT taggables_tag_id_foreign FOREIGN KEY (tag_id) REFERENCES public.tags(id) ON DELETE CASCADE;


--
-- Name: tags tags_team_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.tags
    ADD CONSTRAINT tags_team_id_foreign FOREIGN KEY (team_id) REFERENCES public.teams(id) ON DELETE CASCADE;


--
-- Name: team_invitations team_invitations_team_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_invitations
    ADD CONSTRAINT team_invitations_team_id_foreign FOREIGN KEY (team_id) REFERENCES public.teams(id) ON DELETE CASCADE;


--
-- Name: teams teams_avatar_s3_storage_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.teams
    ADD CONSTRAINT teams_avatar_s3_storage_id_foreign FOREIGN KEY (avatar_s3_storage_id) REFERENCES public.s3_storages(id) ON DELETE SET NULL;


--
-- Name: telegram_notification_settings telegram_notification_settings_team_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.telegram_notification_settings
    ADD CONSTRAINT telegram_notification_settings_team_id_foreign FOREIGN KEY (team_id) REFERENCES public.teams(id) ON DELETE CASCADE;


--
-- Name: user_changelog_reads user_changelog_reads_user_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_changelog_reads
    ADD CONSTRAINT user_changelog_reads_user_id_foreign FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: users users_avatar_s3_storage_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.users
    ADD CONSTRAINT users_avatar_s3_storage_id_foreign FOREIGN KEY (avatar_s3_storage_id) REFERENCES public.s3_storages(id) ON DELETE SET NULL;


--
-- Name: volume_backup_s3_replicas volume_backup_s3_replicas_execution_fk; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.volume_backup_s3_replicas
    ADD CONSTRAINT volume_backup_s3_replicas_execution_fk FOREIGN KEY (execution_id) REFERENCES public.scheduled_volume_backup_executions(id) ON DELETE CASCADE;


--
-- Name: volume_backup_s3_replicas volume_backup_s3_replicas_storage_fk; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.volume_backup_s3_replicas
    ADD CONSTRAINT volume_backup_s3_replicas_storage_fk FOREIGN KEY (s3_storage_id) REFERENCES public.s3_storages(id) ON DELETE SET NULL;


--
-- Name: scheduled_volume_backup_s3_storage volume_backup_s3_storage_backup_fk; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_volume_backup_s3_storage
    ADD CONSTRAINT volume_backup_s3_storage_backup_fk FOREIGN KEY (scheduled_volume_backup_id) REFERENCES public.scheduled_volume_backups(id) ON DELETE CASCADE;


--
-- Name: scheduled_volume_backup_s3_storage volume_backup_s3_storage_storage_fk; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.scheduled_volume_backup_s3_storage
    ADD CONSTRAINT volume_backup_s3_storage_storage_fk FOREIGN KEY (s3_storage_id) REFERENCES public.s3_storages(id) ON DELETE CASCADE;


--
-- Name: webhook_notification_settings webhook_notification_settings_team_id_foreign; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook_notification_settings
    ADD CONSTRAINT webhook_notification_settings_team_id_foreign FOREIGN KEY (team_id) REFERENCES public.teams(id) ON DELETE CASCADE;


--
-- PostgreSQL database dump complete
--


