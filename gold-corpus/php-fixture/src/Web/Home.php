<?php
namespace App\Web;

use App\Util\Grid;
use App\Util\Greeter;

class Home
{
    public function __construct(private Greeter $greeter)
    {
    }

    public function run(string $who): string
    {
        $g = new Grid();
        $n = $g->cell(4);
        $out = $this->greeter->hi($who);
        $rows = $this->greeter->seen();
        return $out . $n . Grid::SIZE;
    }

    public function again(): string
    {
        return $this->greeter->hi('x', 'extra', 'toomany');
    }
}
