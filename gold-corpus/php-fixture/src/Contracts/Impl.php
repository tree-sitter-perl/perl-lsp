<?php
namespace App\Contracts;

abstract class Base implements Speaker
{
    public function speak(string $line): string
    {
        return $line;
    }

    abstract protected function tag(): string;
}

class Loud implements Speaker
{
    public function speak(string $line): string
    {
        return strtoupper($line);
    }
}

class Sub extends Base
{
}

class Quiet implements Speaker
{
    public function speak(string $line): string
    {
        return $line;
    }

    public function hush(): void
    {
    }

    protected function tone()
    {
        return 'low';
    }
}
