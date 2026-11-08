<?php
// Writes vendor/composer/installed.json from composer.lock the way Composer's FilesystemRepository does.
$root = $argv[1];
$l = json_decode(file_get_contents("$root/composer.lock"), true);
$pk = []; $dev = [];
foreach ([["packages", false], ["packages-dev", true]] as [$k, $isDev]) {
  foreach ($l[$k] ?? [] as $p) {
    $p["installation-source"] = "dist";
    $p["install-path"] = "../" . $p["name"];
    $pk[] = $p; if ($isDev) $dev[] = $p["name"];
  }
}
usort($pk, fn($a, $b) => strcmp($a["name"], $b["name"]));
@mkdir("$root/vendor/composer", 0777, true);
file_put_contents("$root/vendor/composer/installed.json",
  json_encode(["packages" => $pk, "dev" => true, "dev-package-names" => $dev], JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE) . "\n");
