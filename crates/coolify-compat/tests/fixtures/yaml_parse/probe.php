<?php

// Prints serialize(Yaml::parse()) and Yaml::dump(…, 10, 2) (or the ParseException) of every case:
//   docker cp cases coolify-rs-reference-coolify-1:/tmp/yamlcases
//   docker exec coolify-rs-reference-coolify-1 php /tmp/probe.php /tmp/yamlcases > expected.txt
require '/var/www/html/vendor/autoload.php';
use Symfony\Component\Yaml\Yaml;
$dir = $argv[1];
$files = glob("$dir/*.yaml"); sort($files);
foreach ($files as $f) {
    $in = file_get_contents($f);
    try {
        $v = Yaml::parse($in, maxAliasesForCollections: 256);
        $out = "OK " . serialize($v) . "\n---DUMP---\n" . (is_array($v) ? Yaml::dump($v, 10, 2) : '');
    } catch (Throwable $e) {
        $out = "ERR " . get_class($e) . ': ' . $e->getMessage();
    }
    echo "=== " . basename($f) . "\n" . $out . "\n";
}
