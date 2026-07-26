public interface Greeter {
    String greet(String name);
}

class EnglishGreeter implements Greeter {
    @Override
    public String greet(String name) {
        return "Hello, " + name;
    }
}

class PoliteEnglishGreeter extends EnglishGreeter {
    @Override
    public String greet(String name) {
        return "Good day, " + name;
    }
}
