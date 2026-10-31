<?php
// The reference's side of compat/diff/metrics_diff.py, run with:
//   docker exec -i -e MODE=... -e ARGS=<json> coolify-rs-reference-coolify-1 php < this file
// seed creates two users with their own teams and rows and prints what it made; cleanup deletes exactly
// the ids it is handed; the others run the upstream components and print what they would show.
require '/var/www/html/vendor/autoload.php';
$app = require_once '/var/www/html/bootstrap/app.php';
$app->make(Illuminate\Contracts\Console\Kernel::class)->bootstrap();

use App\Livewire\Dashboard\ServerMetricsChart;
use App\Livewire\Dashboard\TrafficAnalytics;
use App\Livewire\Fleet\Metrics as FleetMetrics;
use App\Livewire\Project\Application\Analytics as ApplicationAnalytics;
use App\Livewire\Project\Service\Analytics as ServiceAnalytics;
use App\Models\Application;
use App\Models\Environment;
use App\Models\PrivateKey;
use App\Models\Project;
use App\Models\Server;
use App\Models\Service;
use App\Models\ServiceApplication;
use App\Models\StandaloneDocker;
use App\Models\StandaloneRedis;
use App\Models\Team;
use App\Models\User;
use Illuminate\Support\Facades\Auth;
use Illuminate\Support\Facades\DB;
use Illuminate\Support\Facades\Session;

$args = json_decode(getenv('ARGS') ?: '{}', true);

function out($value): void
{
    echo "\n@@JSON@@".json_encode($value)."\n";
}

function actAs(int $userId, int $teamId): void
{
    $user = User::find($userId);
    Auth::login($user);
    Session::put('currentTeam', Team::find($teamId));
}

/** Every effect of a component: what the dispatches carried. */
function dispatches($component): array
{
    return $component->dispatched ?? [];
}

switch (getenv('MODE')) {
    case 'seed':
        $ids = ['users' => [], 'teams' => [], 'keys' => [], 'servers' => [], 'dockers' => [], 'projects' => [], 'environments' => [], 'applications' => [], 'redis' => [], 'services' => [], 'service_applications' => []];
        try {
        $alice = DB::table('users')->where('email', 'alice@example.test')->value('password');
        $sourceKey = PrivateKey::withoutGlobalScopes()->find(60);
        $makeUser = function (string $name) use (&$ids, $alice) {
            $user = User::create(['name' => $name, 'email' => "$name@example.test", 'password' => 'x']);
            DB::table('users')->where('id', $user->id)->update(['password' => $alice, 'email_verified_at' => now()]);
            $team = $user->teams()->first();
            DB::table('teams')->where('id', $team->id)->update(['show_boarding' => false]);
            $ids['users'][] = $user->id;
            $ids['teams'][] = $team->id;

            return [$user, $team];
        };
        [$user, $team] = $makeUser('metricsbot');
        [$user2, $team2] = $makeUser('metricsbot2');
        $makeKey = function (Team $t) use (&$ids, $sourceKey) {
            $key = new PrivateKey;
            $key->forceFill(['uuid' => (string) new Visus\Cuid2\Cuid2, 'name' => 'metrics stream', 'private_key' => $sourceKey->private_key, 'team_id' => $t->id, 'fingerprint' => 'metrics-'.$t->id])->saveQuietly();
            $ids['keys'][] = $key->id;

            return $key;
        };
        $key = $makeKey($team);
        $key2 = $makeKey($team2);
        $makeServer = function (string $name, string $ip, Team $t, PrivateKey $k, array $settings, ?string $token) use (&$ids) {
            $server = Server::create(['name' => $name, 'ip' => $ip, 'user' => 'root', 'port' => 22, 'team_id' => $t->id, 'private_key_id' => $k->id]);
            $server->forceFill(['proxy' => ['type' => 'TRAEFIK', 'status' => 'running'], 'sentinel_updated_at' => now()])->saveQuietly();
            $ids['servers'][] = $server->id;
            foreach (StandaloneDocker::where('server_id', $server->id)->get() as $docker) {
                $ids['dockers'][] = $docker->id;
            }
            $values = array_merge(['is_reachable' => true, 'is_usable' => true, 'sentinel_custom_url' => 'http://127.0.0.1:9'], $settings);
            if ($token !== null) {
                $values['sentinel_token'] = $token;
            }
            $row = $server->settings()->first();
            $row->forceFill($values)->saveQuietly();

            return $server;
        };
        $vm = $args['vm'];
        $on = ['is_metrics_enabled' => true, 'is_traffic_analytics_enabled' => true];
        $busy = $makeServer('metrics-busy', $vm, $team, $key, $on, $args['tokens']['busy']);
        $calm = $makeServer('metrics-calm', $vm, $team, $key, $on, $args['tokens']['calm']);
        $old = $makeServer('metrics-old', $vm, $team, $key, ['is_metrics_enabled' => true], $args['tokens']['old']);
        $badToken = $makeServer('metrics-badtoken', $vm, $team, $key, ['is_metrics_enabled' => true], $args['tokens']['bad']);
        $offline = $makeServer('metrics-offline', 'metrics-nohost.invalid', $team, $key, ['is_metrics_enabled' => true, 'is_reachable' => false], $args['tokens']['bad']);
        $noMetrics = $makeServer('metrics-off', $vm, $team, $key, [], null);
        $second = $makeServer('metrics-second-team', $vm, $team2, $key2, ['is_traffic_analytics_enabled' => false], $args['tokens']['busy']);

        $project = new Project;
        $project->forceFill(['name' => 'metrics stream', 'team_id' => $team->id])->save();
        $ids['projects'][] = $project->id;
        $environment = Environment::where('project_id', $project->id)->first();
        foreach (Environment::where('project_id', $project->id)->get() as $e) {
            $ids['environments'][] = $e->id;
        }
        $dockerOf = fn (Server $s) => StandaloneDocker::where('server_id', $s->id)->first();
        $makeApp = function (string $uuid, string $name, Server $s, string $status, array $extra = []) use (&$ids, $environment, $dockerOf) {
            $docker = $dockerOf($s);
            $app = new Application;
            $app->forceFill(array_merge([
                'uuid' => $uuid, 'name' => $name, 'environment_id' => $environment->id, 'destination_id' => $docker->id,
                'destination_type' => StandaloneDocker::class, 'status' => $status, 'git_repository' => 'coollabsio/coolify',
                'git_branch' => 'main', 'build_pack' => 'dockerimage', 'ports_exposes' => '80', 'fqdn' => 'http://'.$name.'.metrics.test',
                'docker_registry_image_name' => 'nginx', 'docker_registry_image_tag' => 'alpine',
                'repository_project_id' => 0,
            ], $extra))->saveQuietly();
            $ids['applications'][] = $app->id;

            return $app;
        };
        $app1 = $makeApp($args['uuids']['app1'], 'app1', $busy, 'running:healthy');
        $app2 = $makeApp($args['uuids']['app2'], 'app2', $calm, 'running:healthy');
        $compose = $makeApp('metricsappcompose00000000', 'compose', $busy, 'running:healthy', ['build_pack' => 'dockercompose']);
        $stopped = $makeApp('metricsappstopped00000000', 'stopped', $busy, 'exited');
        $off = $makeApp('metricsappmetricsoff00000', 'off', $noMetrics, 'running:healthy');
        $redis = new StandaloneRedis;
        $redis->forceFill(['uuid' => $args['uuids']['db'], 'name' => 'redis', 'environment_id' => $environment->id, 'destination_id' => $dockerOf($busy)->id, 'destination_type' => StandaloneDocker::class, 'status' => 'running:healthy', 'image' => 'redis:alpine'])->saveQuietly();
        $ids['redis'][] = $redis->id;
        $service = new Service;
        $service->forceFill(['uuid' => $args['uuids']['service'], 'name' => 'svc', 'environment_id' => $environment->id, 'server_id' => $busy->id, 'destination_id' => $dockerOf($busy)->id, 'destination_type' => StandaloneDocker::class, 'docker_compose_raw' => "services:\n  web:\n    image: nginx:alpine\n"])->saveQuietly();
        $ids['services'][] = $service->id;
        $serviceApp = new ServiceApplication;
        $serviceApp->forceFill(['uuid' => 'metricssvcapp100000000000', 'name' => 'web', 'service_id' => $service->id, 'fqdn' => 'http://svc.metrics.test', 'image' => 'nginx:alpine', 'status' => 'running:healthy'])->saveQuietly();
        $ids['service_applications'][] = $serviceApp->id;
        $serviceOff = new Service;
        $serviceOff->forceFill(['uuid' => 'metricssvcoff000000000000', 'name' => 'svc-off', 'environment_id' => $environment->id, 'server_id' => $noMetrics->id, 'destination_id' => $dockerOf($noMetrics)->id, 'destination_type' => StandaloneDocker::class, 'docker_compose_raw' => "services:\n  web:\n    image: nginx:alpine\n"])->saveQuietly();
        $ids['services'][] = $serviceOff->id;

        out([
            'ids' => $ids,
            'user' => $user->id, 'team' => $team->id, 'user2' => $user2->id, 'team2' => $team2->id,
            'project' => $project->uuid, 'environment' => $environment->uuid,
            'servers' => ['busy' => $busy->uuid, 'calm' => $calm->uuid, 'old' => $old->uuid, 'badtoken' => $badToken->uuid, 'offline' => $offline->uuid, 'off' => $noMetrics->uuid, 'second' => $second->uuid],
            'server_ids' => ['busy' => $busy->id, 'calm' => $calm->id, 'old' => $old->id, 'badtoken' => $badToken->id, 'offline' => $offline->id, 'off' => $noMetrics->id, 'second' => $second->id],
            'labels' => ['app1' => $app1->id, 'app2' => $app2->id, 'db' => $redis->id, 'service' => $service->id, 'service_app' => $serviceApp->id],
            'other_team_app' => Application::whereHas('environment.project', fn ($q) => $q->where('team_id', 4))->value('uuid'),
        ]);
        } catch (Throwable $e) {
            out(['seed_error' => substr($e->getMessage(), 0, 300), 'ids' => $ids]);
        }
        break;

    case 'cleanup':
        // Keyed to the ids seed printed, nothing else. Order follows the foreign keys.
        $i = $args['ids'];
        DB::table('service_applications')->whereIn('id', $i['service_applications'])->delete();
        DB::table('services')->whereIn('id', $i['services'])->delete();
        DB::table('standalone_redis')->whereIn('id', $i['redis'])->delete();
        DB::table('application_settings')->whereIn('application_id', $i['applications'])->delete();
        DB::table('applications')->whereIn('id', $i['applications'])->delete();
        DB::table('environments')->whereIn('id', $i['environments'])->delete();
        DB::table('project_settings')->whereIn('project_id', $i['projects'])->delete();
        DB::table('projects')->whereIn('id', $i['projects'])->delete();
        DB::table('standalone_dockers')->whereIn('id', $i['dockers'])->delete();
        DB::table('ssl_certificates')->whereIn('server_id', $i['servers'])->delete();
        DB::table('server_settings')->whereIn('server_id', $i['servers'])->delete();
        DB::table('servers')->whereIn('id', $i['servers'])->delete();
        DB::table('private_keys')->whereIn('id', $i['keys'])->delete();
        foreach (['email_notification_settings', 'discord_notification_settings', 'slack_notification_settings', 'telegram_notification_settings', 'pushover_notification_settings', 'webhook_notification_settings'] as $table) {
            DB::table($table)->whereIn('team_id', $i['teams'])->delete();
        }
        DB::table('team_user')->whereIn('team_id', $i['teams'])->delete();
        DB::table('teams')->whereIn('id', $i['teams'])->delete();
        DB::table('user_changelog_reads')->whereIn('user_id', $i['users'])->delete();
        DB::table('users')->whereIn('id', $i['users'])->delete();
        out(['deleted' => true]);
        break;

    case 'touch':
        // Keeps the scheduler from taking the Sentinel of these rows for a stalled one.
        DB::table('servers')->whereIn('id', $args['ids'])->update(['sentinel_updated_at' => now()]);
        out(['ok' => true]);
        break;

    case 'fleet':
        actAs($args['user'], $args['team']);
        $c = new FleetMetrics;
        $c->range = $args['range'] ?? '24h';
        $c->serverUuid = $args['server'] ?? '';
        $c->containerMetric = $args['cmetric'] ?? 'cpu';
        $c->mount();
        $containers = array_map(fn ($row) => array_merge($row, $c->containerMeta($row['id'])), $c->topContainers);
        out([
            'hasServers' => $c->servers->isNotEmpty(), 'enableableServers' => $c->enableableServers, 'serverOptions' => $c->serverOptions,
            'range' => $c->range, 'serverUuid' => $c->serverUuid, 'containerMetric' => $c->containerMetric,
            'kpis' => $c->kpis, 'serverRows' => $c->serverRows, 'top' => $containers, 'chartData' => $c->chartData,
        ]);
        break;

    case 'server_series':
        actAs($args['user'], $args['team']);
        $server = Server::where('uuid', $args['uuid'])->first();
        $mins = $args['mins'];
        $network = rescue(fn () => $server->getNetworkMetrics($mins), null, false) ?? [];
        out([
            'cpuSeries' => $server->getCpuMetrics($mins), 'memorySeries' => $server->getMemoryMetrics($mins),
            'diskSeries' => rescue(fn () => $server->getDiskMetrics($mins), null, false), 'loadSeries' => rescue(fn () => $server->getLoadMetrics($mins), null, false),
            'networkRxSeries' => $network['rx'] ?? [], 'networkTxSeries' => $network['tx'] ?? [],
        ]);
        break;

    case 'resource_series':
        actAs($args['user'], $args['team']);
        $resource = $args['kind'] === 'database' ? StandaloneRedis::where('uuid', $args['uuid'])->first() : Application::where('uuid', $args['uuid'])->first();
        $resource->prefetchMetrics(['cpu', 'memory'], $args['mins']);
        out(['cpu' => $resource->getCpuMetrics($args['mins']), 'memory' => $resource->getMemoryMetrics($args['mins'])]);
        break;

    case 'dashboard_chart':
        actAs($args['user'], $args['team']);
        $server = Server::where('uuid', $args['uuid'])->first();
        if (! $server->isFunctional()) {
            out(null);
            break;
        }
        $server->prefetchMetrics(['cpu', 'memory'], 10);
        out(['cpu' => $server->getCpuMetrics(10), 'memory' => $server->getMemoryMetrics(10)]);
        break;

    case 'dashboard_traffic':
        actAs($args['user'], $args['team']);
        $c = new TrafficAnalytics;
        $c->range = $args['range'] ?? '24h';
        $c->mount();
        out(['serverCount' => $c->servers->count(), 'range' => $c->range, 'overview' => $c->overview, 'uniquesApproximate' => $c->uniquesApproximate, 'series' => $c->series, 'errorRate' => $c->errorRate(), 'bandwidthBytes' => $c->bandwidthBytes()]);
        break;

    case 'resource_analytics':
        actAs($args['user'], $args['team']);
        if ($args['kind'] === 'service') {
            $c = new ServiceAnalytics;
            $c->service = Service::where('uuid', $args['uuid'])->first();
        } else {
            $c = new ApplicationAnalytics;
            $c->application = Application::where('uuid', $args['uuid'])->first();
        }
        $c->range = $args['range'] ?? '24h';
        $c->mount();
        $device = $c->deviceChartData();
        out(['enabled' => $c->enabled, 'analyticsServerUuid' => $c->analyticsServerUuid, 'range' => $c->range, 'chartId' => $c->chartId, 'overview' => $c->overview, 'ipMode' => $c->ipMode, 'latencyApproximate' => $c->latencyApproximate, 'uniquesApproximate' => $c->uniquesApproximate, 'topPaths' => $c->topPaths, 'breakdowns' => $c->breakdowns, 'attribution' => $c->attribution, 'series' => $c->series, 'errorRate' => $c->errorRate(), 'bandwidthBytes' => $c->bandwidthBytes(), 'device' => $device]);
        break;

    default:
        fwrite(STDERR, "unknown mode\n");
        exit(2);
}
