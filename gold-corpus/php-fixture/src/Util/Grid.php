<?php
namespace App\Util;

class Grid
{
    public const SIZE = 3;
    public static int $count = 0;

    public function cell(int $i): int
    {
        return $i % self::SIZE;
    }
}
