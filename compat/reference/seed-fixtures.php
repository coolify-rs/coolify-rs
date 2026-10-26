<?php
// Seeds deterministic API fixtures into the *reference* instance using
// upstream's own models, so rows are exactly what upstream would write.
// Run via scripts/reference-seed.sh. Prints tokens as JSON on stdout.
use App\Models\InstanceSettings;
use App\Models\Team;
use App\Models\User;
use Illuminate\Support\Facades\Hash;

$pw = Hash::make(bin2hex(random_bytes(16)));
$root = User::find(0) ?? tap(new User(['name' => 'Root', 'email' => 'root@example.test']), function ($u) use ($pw) {
    $u->id = 0; $u->password = $pw; $u->save();
});
if (! $root->teams()->where('teams.id', 0)->exists()) { $root->teams()->attach(0, ['role' => 'owner']); }

$mk = fn ($name, $email) => User::where('email', $email)->first()
    ?? User::forceCreate(['name' => $name, 'email' => $email, 'password' => $pw]);
$owner = $mk('Alice Owner', 'alice@example.test');
$member = $mk('Bob Member', 'bob@example.test');
$other = $mk('Carol Other', 'carol@example.test');

$acme = Team::where('name', 'Acme')->first() ?? Team::forceCreate(['name' => 'Acme', 'description' => 'Fixture team', 'personal_team' => false]);
foreach ([[$owner, 'owner'], [$member, 'member']] as [$u, $role]) {
    if (! $u->teams()->where('teams.id', $acme->id)->exists()) { $u->teams()->attach($acme->id, ['role' => $role]); }
}
$carolTeam = $other->teams()->first();

$s = InstanceSettings::get(); $s->is_api_enabled = true; $s->allowed_ips = null; $s->save();

$tok = fn ($u, $team, $abilities) => $u->createToken('fixture-'.implode('-', $abilities), $abilities, null, $team)->plainTextToken;
echo json_encode([
    'acme_team_id' => $acme->id,
    'carol_team_id' => $carolTeam->id,
    'root_root' => $tok($root, 0, ['root']),
    'root_read' => $tok($root, 0, ['read']),
    'acme_owner_root' => $tok($owner, $acme->id, ['root']),
    'acme_owner_read' => $tok($owner, $acme->id, ['read']),
    'acme_owner_write' => $tok($owner, $acme->id, ['write']),
    'acme_owner_read_write' => $tok($owner, $acme->id, ['read', 'write']),
    'acme_member_read' => $tok($member, $acme->id, ['read']),
    'acme_member_write_legacy' => $tok($member, $acme->id, ['write']),
    'carol_root' => $tok($other, $carolTeam->id, ['root']),
], JSON_PRETTY_PRINT);
