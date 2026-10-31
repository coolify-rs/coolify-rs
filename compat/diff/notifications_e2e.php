<?php

// The reference side of compat/diff/notifications_e2e.py. Reads /tmp/notif-e2e.json (cmd and its
// arguments), writes /tmp/notif-e2e-out.json. Runs in tinker, delivers with the sync queue.

use App\Models\Team;
use App\Models\User;
use Illuminate\Support\Facades\DB;
use Illuminate\Support\Facades\Http;

define('NOTIF_LIBRARY_ONLY', true);
require '/tmp/notif-ref.php';

$arg = json_decode(file_get_contents('/tmp/notif-e2e.json'), true);
config(['queue.default' => 'sync']);

function e2e_marker($listener, $name)
{
    Http::post("{$listener}/__case/{$name}", []);
}

/** Telegram and Pushover have fixed hosts upstream: forward them to the listener, nothing else leaves. */
function e2e_fake_external($listener)
{
    Http::fake([
        'api.telegram.org/*' => function ($request) use ($listener) {
            $path = parse_url($request->url(), PHP_URL_PATH);
            e2e_forward("{$listener}/tg{$path}", $request->body());

            return Http::response(['ok' => true]);
        },
        'api.pushover.net/*' => function ($request) use ($listener) {
            $path = parse_url($request->url(), PHP_URL_PATH);
            e2e_forward("{$listener}/po{$path}", $request->body());

            return Http::response(['ok' => true]);
        },
    ]);
}

function e2e_forward($url, $body)
{
    $ch = curl_init($url);
    curl_setopt_array($ch, [CURLOPT_POST => true, CURLOPT_POSTFIELDS => $body, CURLOPT_RETURNTRANSFER => true, CURLOPT_HTTPHEADER => ['Content-Type: application/json', 'User-Agent: forwarded']]);
    curl_exec($ch);
    curl_close($ch);
}

$out = [];

switch ($arg['cmd']) {
    case 'setup':
        // A team with a member, all events on, secrets stored encrypted by the models.
        $team = Team::forceCreate(['name' => 'notif-e2e', 'description' => 'notifications e2e', 'personal_team' => false]);
        $before = Team::query()->pluck('id')->all();
        $user = User::forceCreate(['name' => 'notif e2e', 'email' => $arg['email'], 'password' => bcrypt(bin2hex(random_bytes(16)))]);
        $personal = array_values(array_diff(Team::query()->pluck('id')->all(), $before));
        $team->members()->attach($user->id, ['role' => 'owner']);
        foreach (['email', 'discord', 'slack', 'telegram', 'pushover', 'webhook'] as $channel) {
            $settings = $team->getNotificationSettings($channel);
            foreach ($settings->getFillable() as $column) {
                if (str_ends_with($column, "_{$channel}_notifications")) {
                    $settings->{$column} = true;
                }
            }
            $settings->save();
        }
        $out = ['team_id' => $team->id, 'user_id' => $user->id, 'personal_teams' => $personal];
        break;

    case 'configure':
        // Point the team's channels at the given hosts. saveQuietly keeps other rows alone.
        $team = Team::find($arg['team_id']);
        $h = $arg['listener'];
        $team->discordNotificationSettings->forceFill(['discord_enabled' => true, 'discord_ping_enabled' => true, 'discord_webhook_url' => $arg['discord_url'] ?? "{$h}/discord/secret", 'failure_count' => 0])->saveQuietly();
        $team->slackNotificationSettings->forceFill(['slack_enabled' => true, 'slack_webhook_url' => "{$h}/slack/secret", 'failure_count' => 0])->saveQuietly();
        $team->webhookNotificationSettings->forceFill(['webhook_enabled' => true, 'webhook_url' => "{$h}/webhook/secret", 'failure_count' => 0])->saveQuietly();
        $team->telegramNotificationSettings->forceFill(['telegram_enabled' => true, 'telegram_token' => ' TOKEN ', 'telegram_chat_id' => ' 123 ', 'telegram_notifications_deployment_success_thread_id' => '77', 'failure_count' => 0])->saveQuietly();
        $team->pushoverNotificationSettings->forceFill(['pushover_enabled' => true, 'pushover_user_key' => 'USERKEY', 'pushover_api_token' => 'APITOKEN', 'failure_count' => 0])->saveQuietly();
        $team->emailNotificationSettings->forceFill(array_merge([
            'smtp_enabled' => false, 'resend_enabled' => false, 'use_instance_email_settings' => false,
            'smtp_from_address' => 'from@notif.test', 'smtp_from_name' => 'Notif "E2E"', 'smtp_host' => $arg['smtp_host'],
            'smtp_port' => $arg['smtp_port'], 'smtp_encryption' => 'none', 'smtp_timeout' => 10, 'failure_count' => 0,
            'deployment_commit_details_email_notifications' => true,
        ], $arg['email'] ?? []))->saveQuietly();
        break;

    case 'instance':
        // Instance email and the allow list. Python restores the saved values afterwards.
        $s = App\Models\InstanceSettings::findOrFail(0);
        $s->forceFill($arg['values'])->saveQuietly();
        break;

    case 'send':
        $team = Team::find($arg['team_id']);
        e2e_fake_external($arg['listener']);
        if ($arg['resend_base'] ?? null) {
            putenv("RESEND_BASE_URL={$arg['resend_base']}");
        }
        foreach ($arg['cases'] as $case) {
            e2e_marker($arg['listener'], $case['name']);
            try {
                if (isset($case['notification'])) {
                    Illuminate\Support\Facades\RateLimiter::clear('server-reachable:999000');
                    Illuminate\Support\Facades\RateLimiter::clear('server-unreachable:999000');
                    $team->notifyNow(notif_build($case['notification']));
                } elseif (isset($case['mail'])) {
                    if ($case['mail']['type'] === 'Test') {
                        $team->notifyNow(new App\Notifications\TransactionalEmails\Test($case['to']));
                    } else {
                        $settings = App\Models\InstanceSettings::findOrFail(0);
                        send_user_an_email(notif_mail($case['mail']), $case['to']);
                    }
                }
                $out[$case['name']] = 'ok';
            } catch (Throwable $e) {
                $out[$case['name']] = get_class($e).': '.$e->getMessage();
            }
        }
        // Throttle row of HighDiskUsage if any, on the fake server id.
        DB::table('notification_throttles')->where('notifiable_id', 999000)->delete();
        break;

    case 'state':
        $team = Team::find($arg['team_id']);
        $out = [
            'discord_enabled' => (bool) $team->discordNotificationSettings->discord_enabled,
            'discord_failure_count' => (int) DB::table('discord_notification_settings')->where('team_id', $team->id)->value('failure_count'),
            'audit' => DB::table('audit_events')->where('team_id', $team->id)->where('event', 'like', 'notifications.%')->orderBy('id')->get(['event', 'source', 'action', 'level', 'actor_type', 'description', 'metadata', 'resource_type'])->map(fn ($r) => (array) $r)->all(),
        ];
        break;

    case 'clear_audit':
        DB::table('audit_events')->where('team_id', $arg['team_id'])->where('event', 'like', 'notifications.%')->delete();
        break;

    case 'cleanup':
        DB::table('notification_throttles')->where('notifiable_id', 999000)->delete();
        foreach (['email', 'discord', 'slack', 'telegram', 'pushover', 'webhook'] as $channel) {
            DB::table("{$channel}_notification_settings")->whereIn('team_id', array_merge([$arg['team_id']], $arg['personal_teams']))->delete();
        }
        DB::table('audit_events')->where('team_id', $arg['team_id'])->delete();
        DB::table('team_user')->where('user_id', $arg['user_id'])->delete();
        DB::table('team_user')->whereIn('team_id', array_merge([$arg['team_id']], $arg['personal_teams']))->delete();
        DB::table('audit_events')->whereIn('team_id', $arg['personal_teams'])->delete();
        DB::table('teams')->whereIn('id', array_merge([$arg['team_id']], $arg['personal_teams']))->delete();
        DB::table('users')->where('id', $arg['user_id'])->delete();
        break;
}

file_put_contents('/tmp/notif-e2e-out.json', json_encode($out));
echo "done\n";
