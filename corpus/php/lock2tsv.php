<?php
$l = json_decode(file_get_contents($argv[1]), true);
foreach (array_merge($l["packages"], $l["packages-dev"] ?? []) as $p) {
  $s = $p["source"] ?? null;
  echo $p["name"], "\t", $s["url"] ?? "-", "\t", $s["reference"] ?? "-", "\t", $p["type"] ?? "library", "\n";
}
