<?php
namespace App\Tests;

use App\Util\Greeter;
use PHPUnit\Framework\TestCase;

class GreeterTest extends TestCase
{
    public function testHi(): void
    {
        $m = $this->createMock(Greeter::class);
        $out = $m->hi('bob');
        self::assertSame('hey', $out);
    }
}
