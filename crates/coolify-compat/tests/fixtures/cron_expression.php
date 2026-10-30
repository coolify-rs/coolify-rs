<?php

// Regenerates cron_expression.json from the reference's dragonmantank/cron-expression:
//   docker cp this-file coolify-rs-reference-coolify-1:/tmp/cron_expression.php
//   docker exec coolify-rs-reference-coolify-1 php /tmp/cron_expression.php > cron_expression.json

require '/var/www/html/vendor/autoload.php';

use Cron\CronExpression;

$expressions = [
    '* * * * *', '*/5 * * * *', '7 * * * *', '0,30 * * * *', '5,*/20 * * * *', '10-20 * * * *',
    '10-40/7 * * * *', '59 23 * * *', '0 0 * * *', '30 2 * * *', '30 1 * * *', '30 3 * * *', '0 2,3 * * *',
    '15 0-5 * * *', '0 */6 * * *', '0 1-3/2 * * *', '0 0 1 * *', '0 0 31 * *', '0 12 L * *',
    '0 12 15W * *', '0 12 1W * *', '0 12 * * 0', '0 12 * * 7', '0 12 * * MON-FRI', '0 12 * * 5-7',
    '0 12 * * 7-2', '0 12 * * FRIL', '0 12 * * 5#2', '0 12 * * 0#1', '0 0 15 * FRI', '0 0 ? * MON',
    '0 0 1 JAN *', '0 0 * FEB,AUG *', '0 0 29 2 *', '@hourly', '@daily', '@weekly', '@monthly', '@yearly',
    '@midnight', '@annually', '0 0 */40 * *', '*/59 * * * *', '*/61 * * * *', '0 */23 * * *', '0 */25 * * *',
];
$zones = ['UTC', 'Asia/Karachi', 'America/New_York', 'Europe/Berlin', 'Australia/Lord_Howe', 'Asia/Kolkata'];
$instants = [
    '2026-03-08T06:59:30Z', '2026-03-08T07:00:00Z', '2026-03-08T07:30:00Z', '2026-03-08T08:15:00Z',
    '2026-11-01T05:30:00Z', '2026-11-01T06:30:00Z', '2026-11-01T07:10:00Z',
    '2026-03-29T00:59:00Z', '2026-03-29T01:00:00Z', '2026-03-29T01:30:00Z', '2026-10-25T00:30:00Z',
    '2026-10-25T01:00:00Z', '2026-10-25T01:30:00Z', '2026-10-04T15:45:00Z', '2026-04-04T15:15:00Z',
    '2026-01-01T00:00:00Z', '2026-02-28T23:59:59Z', '2028-02-29T12:00:00Z', '2026-06-15T12:00:00Z',
    '2026-12-31T23:59:00Z',
];

$rows = [];
foreach ($expressions as $expression) {
    $cron = new CronExpression($expression);
    foreach ($zones as $zone) {
        foreach ($instants as $instant) {
            $time = (new DateTime($instant))->setTimezone(new DateTimeZone($zone));
            $run = function (callable $f) {
                try {
                    return $f()->format('Y-m-d\TH:i:sP');
                } catch (Throwable $e) {
                    return 'error';
                }
            };
            $rows[] = [
                $expression,
                $zone,
                $instant,
                $cron->isDue($time) ? 1 : 0,
                $run(fn () => $cron->getPreviousRunDate($time, 0, true)),
                $run(fn () => $cron->getPreviousRunDate($time, 0, false)),
                $run(fn () => $cron->getNextRunDate($time, 0, true)),
                $run(fn () => $cron->getNextRunDate($time, 0, false)),
            ];
        }
    }
}
echo json_encode($rows);
