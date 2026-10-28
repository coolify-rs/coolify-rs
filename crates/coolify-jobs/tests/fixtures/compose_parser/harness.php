<?php

/*
 * Records what upstream's applicationParser() does for one case, inside a transaction that is
 * rolled back (nothing stays in the reference database), with queued jobs faked.
 *
 *   docker cp <dir> coolify-rs-reference-coolify-1:/tmp/compose_parser
 *   docker exec coolify-rs-reference-coolify-1 php /tmp/compose_parser/harness.php /tmp/compose_parser/cases/<case>.json
 *
 * Prints one JSON document: the rows the Rust test seeds (`context`), the state before the parse
 * (`before`) and after each parse (`steps`). Secrets of the reference instance are not printed:
 * encrypted columns are replaced by `redacted` and variable values are printed decrypted (they are test values).
 */

require '/var/www/html/vendor/autoload.php';
$app = require '/var/www/html/bootstrap/app.php';
$app->make(Illuminate\Contracts\Console\Kernel::class)->bootstrap();

use App\Models\Application;
use App\Models\EnvironmentVariable;
use App\Models\LocalFileVolume;
use App\Models\LocalPersistentVolume;
use App\Models\Project;
use App\Models\Server;
use App\Models\SharedEnvironmentVariable;
use Carbon\Carbon;
use Illuminate\Support\Facades\Bus;
use Illuminate\Support\Facades\DB;
use Illuminate\Support\Facades\Queue;
use Symfony\Component\Yaml\Yaml;

$caseFile = $argv[1];
$case = json_decode(file_get_contents($caseFile), true);
$compose = $case['compose'] ?? null;
if (isset($case['compose_file'])) {
    $compose = file_get_contents(dirname($caseFile, 2).'/composes/'.$case['compose_file']);
}

Bus::fake();
Queue::fake();

$row = fn (string $table, int $id) => (array) DB::table($table)->where('id', $id)->first();
$scrub = function (array $r): array {
    foreach ($r as $k => $v) {
        if ($v !== null && preg_match('/(^|_)(token|api_key|password|license_key)$|^manual_webhook_secret_/', $k)) {
            $r[$k] = 'redacted';
        }
    }

    return $r;
};

DB::beginTransaction();
$out = ['name' => $case['name'], 'mode' => $case['mode'] ?? 'parse'];
try {
    $serverId = $case['server_id'] ?? 21;
    $server = Server::find($serverId);
    if (isset($case['proxy_type'])) {
        $server->proxy->set('type', $case['proxy_type']);
        $server->save();
    }
    if (! empty($case['server_settings'])) {
        $server->settings->forceFill($case['server_settings'])->save();
    }
    $project = Project::create(['name' => 'Compose Parser Diff', 'team_id' => 4]);
    $environment = $project->environments()->where('name', 'production')->first();
    foreach ($case['shared_variables'] ?? [] as $shared) {
        SharedEnvironmentVariable::create([
            'key' => $shared['key'], 'value' => $shared['value'], 'type' => $shared['type'],
            'team_id' => 4, 'project_id' => $project->id, 'environment_id' => $environment->id,
        ]);
    }
    $destination = $server->standaloneDockers()->first();
    $attributes = array_merge([
        'name' => 'compose-parser-diff',
        'build_pack' => 'dockercompose',
        'git_repository' => 'https://github.com/coollabsio/coolify-examples',
        'git_branch' => 'main',
        'ports_exposes' => '80',
        'environment_id' => $environment->id,
        'destination_id' => $destination->id,
        'destination_type' => $destination->getMorphClass(),
        'docker_compose_raw' => $compose,
    ], $case['app'] ?? []);
    $application = Application::create($attributes);
    $application->forceFill(['uuid' => $case['uuid'] ?? 'composeparserdiff0000001'])->save();
    if (! empty($case['settings'])) {
        $application->settings->forceFill($case['settings'])->save();
    }
    foreach ($case['existing_env'] ?? [] as $variable) {
        EnvironmentVariable::create(array_merge([
            'resourceable_type' => Application::class,
            'resourceable_id' => $application->id,
        ], $variable));
    }
    foreach ($case['existing_files'] ?? [] as $file) {
        LocalFileVolume::create(array_merge([
            'resource_type' => Application::class,
            'resource_id' => $application->id,
        ], $file));
    }
    foreach ($case['existing_volumes'] ?? [] as $volume) {
        LocalPersistentVolume::forceCreate(array_merge([
            'resource_type' => Application::class,
            'resource_id' => $application->id,
        ], $volume));
    }

    $state = function () use ($application, $row, $scrub) {
        $a = (array) DB::table('applications')->where('id', $application->id)->first();

        return [
            'application' => [
                'fqdn' => $a['fqdn'],
                'domain_port_overrides' => $a['domain_port_overrides'],
                'noindex_domains' => $a['noindex_domains'],
                'docker_compose_domains' => $a['docker_compose_domains'],
                'docker_compose' => $a['docker_compose'],
                'docker_compose_raw' => $a['docker_compose_raw'],
            ],
            'environment_variables' => EnvironmentVariable::where('resourceable_type', Application::class)
                ->where('resourceable_id', $application->id)->orderBy('id')->get()
                ->map(fn ($v) => [
                    'key' => $v->key,
                    'value' => $v->getAttributes()['value'] === null ? null : $v->value,
                    'is_preview' => (bool) $v->is_preview,
                    'is_required' => (bool) $v->is_required,
                    'is_shared' => (bool) $v->getAttributes()['is_shared'],
                    'is_runtime' => (bool) $v->is_runtime,
                    'is_buildtime' => (bool) $v->is_buildtime,
                    'is_multiline' => (bool) $v->is_multiline,
                    'is_literal' => (bool) $v->is_literal,
                    'is_shown_once' => (bool) $v->is_shown_once,
                    'preserve_compose_null' => (bool) $v->preserve_compose_null,
                    'comment' => $v->comment,
                    'version' => $v->version,
                ])->all(),
            'file_volumes' => LocalFileVolume::where('resource_type', Application::class)
                ->where('resource_id', $application->id)->orderBy('id')->get()
                ->map(fn ($f) => [
                    'fs_path' => $f->fs_path,
                    'mount_path' => $f->mount_path,
                    'content' => $f->content,
                    'is_directory' => (bool) $f->is_directory,
                    'is_based_on_git' => (bool) $f->is_based_on_git,
                    'is_host_file' => (bool) $f->is_host_file,
                    'is_preview_suffix_enabled' => (bool) $f->is_preview_suffix_enabled,
                ])->all(),
            'persistent_volumes' => LocalPersistentVolume::where('resource_type', Application::class)
                ->where('resource_id', $application->id)->orderBy('id')->get()
                ->map(fn ($v) => [
                    'name' => $v->name,
                    'mount_path' => $v->mount_path,
                    'host_path' => $v->host_path,
                    'is_preview_suffix_enabled' => (bool) $v->is_preview_suffix_enabled,
                    'ignores_compose_driver_options' => (bool) $v->ignores_compose_driver_options,
                ])->all(),
        ];
    };

    $applicationRow = $scrub($row('applications', $application->id));
    $out['context'] = [
        'version' => config('constants.coolify.version'),
        'now' => $case['now'] ?? '2026-01-02 03:04:05',
        'commit' => $case['commit'] ?? null,
        'team' => $row('teams', 4),
        'project' => $row('projects', $project->id),
        'environment' => $row('environments', $environment->id),
        'server' => $scrub($row('servers', $serverId)),
        'server_settings' => $scrub((array) DB::table('server_settings')->where('server_id', $serverId)->first()),
        'standalone_docker' => $row('standalone_dockers', $destination->id),
        'application' => $applicationRow,
        'application_settings' => $scrub((array) DB::table('application_settings')->where('application_id', $application->id)->first()),
        'github_app' => $application->source_type === App\Models\GithubApp::class ? $scrub($row('github_apps', $application->source_id)) : null,
        'shared_variables' => SharedEnvironmentVariable::where('project_id', $project->id)->get()
            ->map(fn ($s) => ['key' => $s->key, 'value' => $s->value, 'type' => $s->type])->all(),
    ];
    $out['before'] = $state();

    Carbon::setTestNow(Carbon::parse($out['context']['now']));
    $out['steps'] = [];
    foreach (range(1, $case['parses'] ?? 1) as $_) {
        $fresh = Application::find($application->id);
        if (($case['mode'] ?? 'parse') === 'load') {
            // The commands with a fixed uuid and what the server answers to them, for the Rust test.
            $fixed = 'composeparserloaduuid001';
            $gitCommands = new ReflectionMethod(Application::class, 'gitCommandsAsShellCommand');
            $readCommands = new ReflectionMethod(Application::class, 'composeFileReadCommands');
            $lsRemote = $gitCommands->invoke($fresh, $fresh->generateGitLsRemoteCommands($fixed, false)['commands']);
            $read = $readCommands->invoke($fresh, $fixed)->all();
            $remote = [];
            foreach (['ls_remote' => [$lsRemote], 'read' => $read] as $what => $commands) {
                try {
                    $output = instant_remote_process($commands, $fresh->destination->server);
                    $remote[$what] = ['output' => $what === 'ls_remote' ? 'ok' : $output];
                } catch (Throwable $e) {
                    $remote[$what] = ['error' => $e->getMessage()];
                }
            }
            instant_remote_process(["rm -rf /tmp/{$fixed}"], $fresh->destination->server, false);
            $out['commands'] = ['ls_remote' => $lsRemote, 'read' => $read];
            $out['remote'] = $remote;
            try {
                $fresh->loadComposeFile(false);
                $step = ['loaded' => true];
            } catch (Throwable $e) {
                $step = ['error' => class_basename($e).': '.$e->getMessage()];
            }
            $out['steps'][] = array_merge($step, $state());

            continue;
        }
        if (($case['mode'] ?? 'parse') === 'raw') {
            // oldRawParser() also runs the mkdir commands; a disabled server refuses them without a connection.
            $server->settings->forceFill(['force_disabled' => true])->save();
            try {
                $fresh->oldRawParser();
                $step = ['raw' => $fresh->docker_compose_raw];
            } catch (Throwable $e) {
                $step = ['error' => $e->getMessage()];
            }
            $out['steps'][] = $step;

            continue;
        }
        try {
            $result = $fresh->parse(0, null, $out['context']['commit']);
            $step = ['result' => Yaml::dump(convertToArray($result), 10, 2), 'warnings' => $fresh->composeVolumeWarnings()];
        } catch (Throwable $e) {
            $step = ['error' => $e->getMessage()];
        }
        $out['steps'][] = array_merge($step, $state());
    }
} finally {
    DB::rollBack();
}

echo json_encode($out, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE)."\n";
