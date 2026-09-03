<?php
namespace PHPUnit\Framework;

abstract class TestCase
{
    protected function createMock(string $originalClassName): object
    {
        return new \stdClass();
    }

    public static function assertSame(mixed $expected, mixed $actual, string $message = ''): void
    {
    }
}
