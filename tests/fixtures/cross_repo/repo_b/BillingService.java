public class BillingService {
    private int amount;

    public BillingService(int amount) {
        this.amount = amount;
    }

    public int charge() {
        return amount;
    }
}
