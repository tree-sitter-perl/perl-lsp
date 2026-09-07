<?php
namespace App\Facade;

use App\Util\Greeter;

/**
 * @method static Greeter greeter()
 * @method Greeter make(string $name)
 */
class Store
{
    public function use(): string
    {
        $g = $this->make('x');
        return $g->hi('y') . self::greeter()->hi('z');
    }
}
