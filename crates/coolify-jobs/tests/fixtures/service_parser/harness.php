<?php

/*
 * Records what upstream's serviceParser() does for one case, inside a transaction that is rolled
 * back (nothing stays in the reference database), with queued jobs faked.
 *
 *   python3 record.py    (copies this directory into the reference container and runs every case)
 *
 * Prints one JSON document: the rows the Rust test seeds (`context`) and the state after each step
 * (`steps`). Encrypted columns are never printed; variable values are printed decrypted (they are
 * generated test values, compared by key, length and character class).
 */

require '/var/www/html/vendor/autoload.php';
$app = require '/var/www/html/bootstrap/app.php';
$app->make(Illuminate\Contracts\Console\Kernel::class)->bootstrap();

use App\Models\EnvironmentVariable;
use App\Models\LocalFileVolume;
use App\Models\LocalPersistentVolume;
use App\Models\Project;
use App\Models\Server;
use App\Models\Service;
use App\Models\ServiceApplication;
use App\Models\ServiceDatabase;
use Carbon\Carbon;
use Illuminate\Support\Facades\DB;
use Illuminate\Support\Facades\Queue;
use Symfony\Component\Yaml\Yaml;

$case = json_decode(file_get_contents($argv[1]), true);
$compose = isset($case['template'])
    ? base64_decode(get_service_templates()->get($case['template'])->compose)
    : (isset($case['compose_file']) ? file_get_contents($case['compose_file']) : $case['compose']);

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
$out = ['name' => $case['name'], 'compose' => $compose];
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
    $project = Project::create(['name' => 'Service Parser Diff', 'team_id' => 4]);
    $environment = $project->environments()->where('name', 'production')->first();
    $destination = $server->standaloneDockers()->first();
    $service = new Service([
        'name' => 'service-parser-diff',
        'docker_compose_raw' => $compose,
        'environment_id' => $environment->id,
        'service_type' => $case['template'] ?? null,
        'server_id' => $serverId,
        'destination_id' => $destination->id,
        'destination_type' => $destination->getMorphClass(),
    ]);
    $service->save();
    $service->forceFill(['uuid' => 'serviceparserdiff0000001'])->save();
    foreach ($case['service'] ?? [] as $k => $v) {
        $service->forceFill([$k => $v])->save();
    }

    $parts = function (string $class, string $table) use ($service) {
        return DB::table($table)->where('service_id', $service->id)->orderBy('id')->get()->map(function ($p) use ($class) {
            $p = (array) $p;
            $uuid = $p['uuid'];
            foreach (['id', 'uuid', 'service_id', 'created_at', 'updated_at', 'last_online_at'] as $k) {
                unset($p[$k]);
            }
            $p['file_volumes'] = LocalFileVolume::where('resource_type', $class)
                ->where('resource_id', DB::table(($class === ServiceApplication::class ? 'service_applications' : 'service_databases'))->where('uuid', $uuid)->value('id'))
                ->orderBy('id')->get()->map(fn ($f) => [
                    'fs_path' => $f->fs_path, 'mount_path' => $f->mount_path, 'content' => $f->content,
                    'is_directory' => (bool) $f->is_directory, 'is_host_file' => (bool) $f->is_host_file,
                ])->all();
            $p['persistent_volumes'] = LocalPersistentVolume::where('resource_type', $class)
                ->where('resource_id', DB::table(($class === ServiceApplication::class ? 'service_applications' : 'service_databases'))->where('uuid', $uuid)->value('id'))
                ->orderBy('id')->get()->map(fn ($v) => ['name' => $v->name, 'mount_path' => $v->mount_path])->all();

            return $p;
        })->all();
    };

    $state = function () use ($service, $parts) {
        $s = (array) DB::table('services')->where('id', $service->id)->first();

        return [
            'part_uuids' => DB::table('service_applications')->where('service_id', $service->id)->pluck('uuid', 'name')
                ->merge(DB::table('service_databases')->where('service_id', $service->id)->pluck('uuid', 'name'))->all(),
            'docker_compose' => $s['docker_compose'],
            'docker_compose_raw' => $s['docker_compose_raw'],
            'applications' => $parts(ServiceApplication::class, 'service_applications'),
            'databases' => $parts(ServiceDatabase::class, 'service_databases'),
            'environment_variables' => EnvironmentVariable::where('resourceable_type', Service::class)
                ->where('resourceable_id', $service->id)->orderBy('id')->get()
                ->map(fn ($v) => [
                    'key' => $v->key,
                    'value' => $v->getAttributes()['value'] === null ? null : $v->value,
                    'is_required' => (bool) $v->is_required,
                    'is_shared' => (bool) $v->getAttributes()['is_shared'],
                    'comment' => $v->comment,
                ])->all(),
        ];
    };

    $out['context'] = [
        'version' => config('constants.coolify.version'),
        'team' => $row('teams', 4),
        'project' => $row('projects', $project->id),
        'environment' => $row('environments', $environment->id),
        'server' => $scrub($row('servers', $serverId)),
        'server_settings' => $scrub((array) DB::table('server_settings')->where('server_id', $serverId)->first()),
        'standalone_docker' => $row('standalone_dockers', $destination->id),
        'service' => $row('services', $service->id),
    ];

    Carbon::setTestNow(Carbon::parse('2026-01-02 03:04:05'));
    $out['steps'] = [];
    foreach ($case['steps'] ?? ['parse', 'parse'] as $action) {
        $fresh = Service::find($service->id);
        try {
            if ($action === 'parse') {
                $result = $fresh->parse();
                $step = ['action' => $action, 'result' => Yaml::dump(convertToArray($result), 10, 2), 'warnings' => $fresh->composeVolumeWarnings()];
            } else {
                // {"name": container, "url": domain}: what saving a domain does.
                $part = $fresh->applications()->where('name', $action['name'])->first();
                $part->fqdn = $action['url'];
                $part->save();
                updateCompose($part);
                $step = ['action' => $action];
            }
        } catch (Throwable $e) {
            $step = ['action' => $action, 'error' => $e->getMessage()];
        }
        $out['steps'][] = array_merge($step, $state());
    }
} finally {
    DB::rollBack();
}

echo json_encode($out, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE)."\n";
