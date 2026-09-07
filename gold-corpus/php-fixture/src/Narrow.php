<?php
namespace App;

use App\Util\Greeter;
use App\Util\Grid;

class Narrow
{
    public function pick(object $p): string
    {
        if ($p instanceof Greeter && !$p instanceof Grid && $p->hi('a')) {
            return $p->hi('b');
        }
        if ($p instanceof Grid) {
            return (string) $p->cell(1);
        }
        return '';
    }
}
