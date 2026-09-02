public class AlphaService {
    private String name;

    public AlphaService(String name) {
        this.name = name;
    }

    public String describe() {
        return "AlphaService: " + name;
    }
}
