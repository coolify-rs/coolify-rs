<?php

// Runs inside the reference (tinker), from compat/diff/notifications_diff.py: builds each
// notification of /tmp/notif-cases.json from unsaved models and writes what each channel sends
// to /tmp/notif-out.json. Nothing is saved and nothing leaves the container (Http::fake).

use App\Jobs\SendMessageToSlackJob;
use App\Jobs\SendMessageToTelegramJob;
use App\Models\Application;
use App\Models\ApplicationDeploymentQueue;
use App\Models\ApplicationPreview;
use App\Models\Environment;
use App\Models\PersonalAccessToken;
use App\Models\Project;
use App\Models\ScheduledDatabaseBackup;
use App\Models\ScheduledTask;
use App\Models\ScheduledVolumeBackup;
use App\Models\ScheduledVolumeBackupExecution;
use App\Models\S3Storage;
use App\Models\Server;
use App\Models\Service;
use App\Models\ServiceApplication;
use App\Models\StandalonePostgresql;
use Illuminate\Support\Facades\Http;
use Illuminate\Support\Facades\RateLimiter;

function notif_raw($model, array $attributes)
{
    $model->setRawAttributes($attributes);

    return $model;
}

function notif_environment($puuid, $euuid, $ename = null, $pname = null)
{
    $e = notif_raw(new Environment, ['uuid' => $euuid, 'name' => $ename]);
    $e->setRelation('project', notif_raw(new Project, ['uuid' => $puuid, 'name' => $pname]));

    return $e;
}

function notif_server($d)
{
    return notif_raw(new Server, ['id' => $d['id'] ?? 999000, 'name' => $d['name'], 'uuid' => $d['uuid']]);
}

function notif_deployment($d, $class)
{
    $app = notif_raw(new Application, ['name' => $d['application_name'], 'uuid' => $d['application_uuid'], 'fqdn' => $d['fqdn']]);
    $app->setRelation('environment', notif_environment($d['project_uuid'], $d['environment_uuid'], $d['environment_name'], $d['project_name']));
    $preview = null;
    if ($d['preview'] ?? null) {
        $preview = notif_raw(new ApplicationPreview, ['pull_request_id' => $d['preview']['pull_request_id'], 'fqdn' => $d['preview']['fqdn']]);
    }
    $n = new $class($app, $d['deployment_uuid'], $preview);
    if ($d['commit'] ?? null) {
        $n->commit_sha = $d['commit']['sha'];
        $n->commit_message = $d['commit']['message'];
        $n->commit_url = $d['commit']['url'];
        $n->commit_branch = $d['commit']['branch'];
    }

    return $n;
}

function notif_db($b)
{
    $db = notif_raw(new StandalonePostgresql, ['name' => $b['name'], 'uuid' => $b['uuid']]);
    $db->setRelation('environment', notif_environment($b['project_uuid'], $b['environment_uuid']));

    return $db;
}

function notif_backup($b, $s3uuid = null)
{
    $backup = notif_raw(new ScheduledDatabaseBackup, ['frequency' => $b['frequency'], 'uuid' => 'bk']);
    $backup->setRelation('s3', $s3uuid ? notif_raw(new S3Storage, ['uuid' => $s3uuid]) : null);

    return $backup;
}

function notif_task($t)
{
    $task = notif_raw(new ScheduledTask, ['name' => $t['name'], 'uuid' => $t['uuid']]);
    $r = $t['resource'] ?? null;
    $task->setRelation('application', null);
    $task->setRelation('service', null);
    if ($r) {
        $model = notif_raw($r['is_application'] ? new Application : new Service, ['uuid' => $r['uuid']]);
        $model->setRelation('environment', notif_environment($r['project_uuid'], $r['environment_uuid']));
        $task->setRelation($r['is_application'] ? 'application' : 'service', $model);
    }

    return $task;
}

function notif_volume($b, array $extra = [])
{
    $backup = new class extends ScheduledVolumeBackup
    {
        public $spec = [];

        public function targetResource(): ?Illuminate\Database\Eloquent\Model
        {
            return $this->spec['resource_name'] === null ? null : notif_raw(new Application, ['name' => $this->spec['resource_name']]);
        }

        public function targetType(): string
        {
            return explode(' ', $this->spec['target'], 2)[0];
        }

        public function targetName(): string
        {
            return explode(' ', $this->spec['target'], 2)[1];
        }

        public function resourceUrl(): ?string
        {
            return $this->spec['url'];
        }
    };
    $backup->spec = $b;
    $backup->setRawAttributes(array_merge(['frequency' => $b['frequency']], $extra));

    return $backup;
}

function notif_build($d)
{
    $type = $d['type'];

    switch ($type) {
        case 'DeploymentSuccess':
            return notif_deployment($d, App\Notifications\Application\DeploymentSuccess::class);
        case 'DeploymentFailed':
            return notif_deployment($d, App\Notifications\Application\DeploymentFailed::class);
        case 'StatusChanged':
            $a = notif_raw(new Application, ['name' => $d['name'], 'uuid' => $d['uuid'], 'fqdn' => $d['fqdn']]);
            $a->setRelation('environment', notif_environment($d['project_uuid'], $d['environment_uuid'], $d['environment_name'], $d['project_name']));

            return new App\Notifications\Application\StatusChanged($a);
        case 'RestartLimitReached':
            $env = notif_environment($d['project_uuid'], $d['environment_uuid'], $d['environment_name'], $d['project_name']);
            $attrs = ['uuid' => $d['resource_uuid'], 'fqdn' => $d['fqdn'], 'restart_count' => $d['restart_count'], 'max_restart_count' => $d['max_restart_count']];
            if ($d['kind'] === 'application') {
                $r = new class extends Application
                {
                    public $max = 0;

                    public function restartLimitMaximum(): int
                    {
                        return $this->max;
                    }
                };
                $r->max = $d['max_restart_count'];
                notif_raw($r, $attrs + ['name' => $d['resource_name']]);
                $r->setRelation('environment', $env);
            } elseif ($d['kind'] === 'preview') {
                [$name, $pr] = explode(' PR #', $d['resource_name']);
                $parent = new class extends Application
                {
                    public $max = 0;

                    public function restartLimitMaximum(): int
                    {
                        return $this->max;
                    }
                };
                $parent->max = $d['max_restart_count'];
                notif_raw($parent, ['name' => $name, 'uuid' => $d['url_uuid']]);
                $parent->setRelation('environment', $env);
                $r = notif_raw(new ApplicationPreview, $attrs + ['pull_request_id' => (int) $pr]);
                $r->setRelation('application', $parent);
            } elseif ($d['kind'] === 'service') {
                $service = notif_raw(new Service, ['uuid' => $d['url_uuid']]);
                $service->setRelation('environment', $env);
                $r = notif_raw(new ServiceApplication, $attrs + ['name' => $d['resource_name']]);
                $r->setRelation('service', $service);
            } else {
                $r = notif_raw(new StandalonePostgresql, $attrs + ['name' => $d['resource_name']]);
                $r->setRelation('environment', $env);
            }

            return new App\Notifications\Application\RestartLimitReached($r);
        case 'ContainerRestarted':
            return new App\Notifications\Container\ContainerRestarted($d['name'], notif_server($d['server']), $d['url'] ?? null);
        case 'BackupSuccess':
            return new App\Notifications\Database\BackupSuccess(notif_backup($d), notif_db($d), $d['database_name']);
        case 'BackupFailed':
            return new App\Notifications\Database\BackupFailed(notif_backup($d['backup']), notif_db($d['backup']), $d['output'], $d['backup']['database_name']);
        case 'BackupMissing':
            $b = notif_raw(new ScheduledDatabaseBackup, ['uuid' => $d['backup_uuid'], 'description' => $d['database_name'], 'missing_backup_notification_days' => $d['days']]);
            $b->setRelation('database', null);

            return new App\Notifications\Database\BackupMissing($b, $d['last_execution_at'] ? Illuminate\Support\Carbon::parse($d['last_execution_at']) : null);
        case 'BackupSuccessWithS3Warning':
            return new App\Notifications\Database\BackupSuccessWithS3Warning(notif_backup($d['backup'], $d['s3_uuid']), notif_db($d['backup']), $d['backup']['database_name'], $d['s3_error']);
        case 'General':
            return new App\Notifications\Internal\GeneralNotification($d['message'], $d['success']);
        case 'TaskSuccess':
            return new App\Notifications\ScheduledTask\TaskSuccess(notif_task($d), $d['output']);
        case 'TaskFailed':
            return new App\Notifications\ScheduledTask\TaskFailed(notif_task($d), $d['output']);
        case 'DockerCleanupSuccess':
            return new App\Notifications\Server\DockerCleanupSuccess(notif_server($d['server']), $d['message']);
        case 'DockerCleanupFailed':
            return new App\Notifications\Server\DockerCleanupFailed(notif_server($d['server']), $d['message']);
        case 'ForceDisabled':
            return new App\Notifications\Server\ForceDisabled(notif_server($d['server']));
        case 'ForceEnabled':
            return new App\Notifications\Server\ForceEnabled(notif_server($d['server']));
        case 'HetznerDeletionFailed':
            return new App\Notifications\Server\HetznerDeletionFailed($d['hetzner_server_id'], 1, $d['error']);
        case 'HighDiskUsage':
            return new App\Notifications\Server\HighDiskUsage(notif_server($d['server']), $d['disk_usage'], $d['threshold']);
        case 'Reachable':
            return new App\Notifications\Server\Reachable(notif_server($d['server']));
        case 'Unreachable':
            return new App\Notifications\Server\Unreachable(notif_server($d['server']));
        case 'ServerPatchCheck':
            return new App\Notifications\Server\ServerPatchCheck(notif_server($d['server']), $d['patch_data']);
        case 'TraefikVersionOutdated':
            $servers = collect($d['servers'])->map(function ($s) {
                $info = array_filter([
                    'current' => $s['current'] ?? null,
                    'latest' => $s['latest'] ?? null,
                    'type' => $s['update_type'] ?? null,
                    'upgrade_target' => $s['upgrade_target'] ?? null,
                    'newer_branch_target' => $s['newer_branch_target'] ?? null,
                    'newer_branch_latest' => $s['newer_branch_latest'] ?? null,
                ], fn ($v) => $v !== null);

                return (object) ['name' => $s['name'], 'uuid' => $s['uuid'], 'outdatedInfo' => $info];
            });

            return new App\Notifications\Server\TraefikVersionOutdated($servers);
        case 'VolumeBackupSuccess':
            return new App\Notifications\VolumeBackup\BackupSuccess(notif_volume($d['backup']), $d['warning']);
        case 'VolumeBackupFailed':
            return new App\Notifications\VolumeBackup\BackupFailed(notif_volume($d['backup']), $d['output']);
        case 'VolumeBackupMissing':
            return new App\Notifications\VolumeBackup\BackupMissing(
                notif_volume($d['backup'], ['uuid' => $d['backup_uuid'], 'missing_backup_notification_days' => $d['days']]),
                $d['last_execution_at'] ? Illuminate\Support\Carbon::parse($d['last_execution_at']) : null
            );
        case 'VolumeBackupRecoveryFailed':
            $e = notif_raw(new ScheduledVolumeBackupExecution, ['uuid' => $d['execution_uuid'], 'recovery_error' => $d['recovery_error'], 'stop_recovery_pending' => $d['containers_stopped']]);
            $e->setRelation('scheduledVolumeBackup', $d['backup'] ? notif_volume($d['backup']) : null);

            return new App\Notifications\VolumeBackup\RecoveryFailed($e);
        case 'ApiTokenExpiring':
            return new App\Notifications\ApiTokenExpiringNotification(notif_raw(new PersonalAccessToken, ['name' => $d['token_name'], 'expires_at' => $d['expires_at']]));
        case 'SslExpiration':
            return new App\Notifications\SslExpirationNotification(collect($d['resources'])->map(fn ($r) => notif_db($r)));
        case 'Test':
            return new App\Notifications\Test($d['emails'], $d['channel'], $d['ping'] ?? false);
    }

    throw new Exception("unknown type {$type}");
}

function notif_mail($d)
{
    switch ($d['type']) {
        case 'ResetPassword':
            $n = new App\Notifications\TransactionalEmails\ResetPassword($d['token']);
            $user = notif_raw(new App\Models\User, ['email' => $d['email']]);
            $reflect = new ReflectionMethod($n, 'buildMailMessage');
            $url = (new ReflectionMethod($n, 'resetUrl'))->invoke($n, $user);
            $settings = clone $n->settings;
            $settings->smtp_from_address = 'a@b.c';
            $n->settings = $settings;

            return $reflect->invoke($n, $url);
        case 'TeamInvitation':
            $mail = new Illuminate\Notifications\Messages\MailMessage;
            $mail->view('emails.invitation-link', ['team' => $d['team'], 'invitation_link' => $d['invitation_link']]);
            $mail->subject('You have been invited to '.$d['team'].' on '.config('app.name').'.');

            return $mail;
        case 'InvitationLink':
            $mail = new Illuminate\Notifications\Messages\MailMessage;
            $mail->subject('Coolify: Invitation for '.$d['team']);
            $mail->view('emails.invitation-link', ['team' => $d['team'], 'email' => 'x@y.z', 'invitation_link' => $d['invitation_link']]);

            return $mail;
        case 'EmailChangeVerification':
            return (new App\Notifications\TransactionalEmails\EmailChangeVerification(new App\Models\User, $d['verification_code'], $d['new_email'], now()))->toMail();
        case 'Test':
            return (new App\Notifications\TransactionalEmails\Test('x@y.z'))->toMail();
        case 'ChannelDisabled':
            $mail = new Illuminate\Notifications\Messages\MailMessage;
            $mail->subject("Coolify: {$d['channel']} notifications were disabled");
            $mail->view('emails.notification-channel-disabled', ['channel' => $d['channel'], 'failures' => $d['failures'], 'reason' => $d['reason'], 'url' => $d['url']]);

            return $mail;
    }

    throw new Exception('unknown mail');
}

/** The body of the last faked request of $send. */
function notif_body(Closure $send): string
{
    Http::fake(['*' => Http::response(['ok' => true])]);
    $send();
    $recorded = Http::recorded();

    return $recorded[count($recorded) - 1][0]->body();
}

function notif_render($n, $details)
{
    $event = null;
    $notifiable = new class
    {
        public $event;

        public $emailNotificationSettings;

        public function getEnabledChannels($e)
        {
            $this->event = $e;

            return [];
        }
    };
    $notifiable->emailNotificationSettings = (object) ['deployment_commit_details_email_notifications' => $details];
    if (! ($n instanceof App\Notifications\Test)) {
        $n->via($notifiable);
        $event = $notifiable->event;
    } else {
        $event = 'test';
    }
    $out = ['subject' => null, 'html' => null];
    if (method_exists($n, 'toMail')) {
        $m = $n->toMail($notifiable);
        $out['subject'] = $m->subject;
        $out['html'] = (string) $m->render();
    }
    $slack = $n->toSlack();
    $tg = $n->toTelegram();
    $discord = $n->toDiscord();
    $push = $n->toPushover();
    $webhook = $n->toWebhook();
    $out['discord_body'] = notif_body(fn () => Http::post('http://x', $discord->toPayload()));
    $job = new SendMessageToSlackJob($slack, 'https://hooks.slack.com/services/x');
    $out['slack_body'] = notif_body(fn () => (new ReflectionMethod($job, 'sendToSlack'))->invoke($job, []));
    $out['mattermost_body'] = notif_body(fn () => (new ReflectionMethod($job, 'sendToMattermost'))->invoke($job, []));
    $telegram = new SendMessageToTelegramJob($tg['message'], $tg['buttons'] ?? [], ' T ', ' C ', '7');
    $out['telegram_body'] = notif_body(fn () => $telegram->sendMessage());
    $out['pushover_body'] = notif_body(fn () => Http::post('http://x', $push->toPayload('T', 'U')));
    $out['webhook_body'] = notif_body(fn () => Http::post('http://x', $webhook));
    $out['event'] = $event;

    return $out;
}

if (defined('NOTIF_LIBRARY_ONLY')) {
    return;
}

$input = json_decode(file_get_contents('/tmp/notif-cases.json'), true);
$output = [
    '__ctx' => [
        'base_url' => base_url(),
        'app_url' => config('app.url'),
        'app_name' => config('app.name'),
        'version' => config('constants.coolify.version'),
    ],
];
foreach ($input['cases'] as $case) {
    $name = $case['name'];
    try {
        if (isset($case['notification'])) {
            RateLimiter::clear('server-reachable:999000');
            RateLimiter::clear('server-unreachable:999000');
            $output[$name] = notif_render(notif_build($case['notification']), $case['commit_details'] ?? false);
        } elseif (isset($case['mail'])) {
            $m = notif_mail($case['mail']);
            $output[$name] = ['subject' => $m->subject, 'html' => (string) $m->render()];
        } else {
            $k = $case['commit'];
            $app = notif_raw(new Application, ['name' => 'a', 'uuid' => 'u', 'git_repository' => $k['git_repository'], 'git_branch' => $k['git_branch']]);
            $app->setRelation('environment', notif_environment('p', 'e', 'n', 'pn'));
            $app->setRelation('source', $k['html_url'] ? (object) ['html_url' => $k['html_url']] : null);
            $q = notif_raw(new ApplicationDeploymentQueue, ['commit' => $k['commit'], 'commit_message' => $k['commit_message']]);
            $preview = $k['is_preview'] ? notif_raw(new ApplicationPreview, ['pull_request_id' => 1]) : null;
            $n = new App\Notifications\Application\DeploymentSuccess($app, 'd', $preview, $q);
            $output[$name] = ['sha' => $n->commit_sha, 'message' => $n->commit_message, 'url' => $n->commit_url, 'branch' => $n->commit_branch];
        }
    } catch (Throwable $e) {
        $output[$name] = ['__error' => get_class($e).': '.$e->getMessage().' at '.$e->getFile().':'.$e->getLine()];
    }
}
RateLimiter::clear('server-reachable:999000');
RateLimiter::clear('server-unreachable:999000');
file_put_contents('/tmp/notif-out.json', json_encode($output, JSON_UNESCAPED_UNICODE | JSON_UNESCAPED_SLASHES));
echo "done\n";
