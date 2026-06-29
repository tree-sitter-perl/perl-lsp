from typing import Optional
import os

class Animal:
    def __init__(self, name: str):
        self.name = name
        self.age = 0
    def speak(self) -> str:
        return self.name

class Dog(Animal):
    def speak(self) -> str:
        return "woof"
    def fetch(self):
        return self.name

def make(n: str) -> Dog:
    d = Dog(n)
    return d

def use():
    d = Dog("rex")
    d.
    x = make("a")
    x.
